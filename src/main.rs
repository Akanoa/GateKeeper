use crate::states::{EndConnection, State};
use crate::Command::Other;
use eyre::{eyre, Context};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::process::ExitStatus;
use std::sync::Arc;
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

mod states;

#[derive(Deserialize, Debug, Clone)]
struct CommandDetails {
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
}

fn check_biscuit(token: &str) -> eyre::Result<String> {
    let public_key_str = include_str!("../assets/public.key");
    let public_key = biscuit_auth::PublicKey::from_bytes_hex(public_key_str)?;

    let biscuit = biscuit_auth::Biscuit::from_base64(token, |_| Ok(public_key))?;

    let mut authorizer = biscuit_auth::macros::authorizer!(r#"allow if true"#);

    authorizer
        .add_token(&biscuit)
        .wrap_err("Unable to add token")?;

    authorizer.authorize().wrap_err("Unable to authorize")?;

    let identifier: Vec<(String,)> = authorizer
        .query("_($id) <- id($id)")
        .wrap_err("Unable to query")?;
    let identifier = identifier
        .first()
        .ok_or(eyre!("Unable to found id"))?
        .clone()
        .0;
    Ok(identifier)
}

fn get_command(token: &str) -> eyre::Result<CommandDetails> {
    let data = include_str!("../assets/credentials.json");
    let credentials = serde_json::from_str::<HashMap<String, CommandDetails>>(data)?;
    let identifier = check_biscuit(token)?;
    let command = credentials
        .get(&identifier)
        .ok_or(eyre!("Identifier {identifier} not found"))?
        .clone();
    Ok(command)
}

async fn format_process_exit_status(
    exit_status: tokio::io::Result<ExitStatus>,
    pid: &str,
    failer: Arc<tokio::sync::Notify>,
) {
    match exit_status {
        Ok(status) => match status.code() {
            None => {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                failer.notify_one();
                log::warn!("Child {pid} process was terminated by a system signal")
            }
            Some(code) => {
                if !status.success() {
                    failer.notify_one();
                    log::warn!("Child {pid} exited with status code {code}")
                } else {
                    log::debug!("Child {pid} successfully terminated")
                }
            }
        },
        Err(err) => {
            failer.notify_one();
            log::warn!("Child process {pid} has exited with error : {err}")
        }
    }
}

async fn handle_process_termination(child: &mut Child) {
    if let Some(pid) = child.id() {
        let pid = pid as i32;
        let mut current_attempt = 0;
        let max_attempts = 3;
        let mut process_alive = true;
        loop {
            if current_attempt > max_attempts {
                break;
            }

            let attempt_kill_status = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                Some(nix::sys::signal::SIGTERM),
            );

            if let Err(err) = child.wait().await {
                log::error!("Unable to wait process with PID {pid} to die : {err}")
            }

            match attempt_kill_status {
                Ok(_) => {
                    log::debug!("[Retry {current_attempt}/{max_attempts}] Successfully gracefull termination of process with PID {pid}");
                    process_alive = false;
                    break;
                }
                Err(err) => {
                    log::warn!("[Retry {current_attempt}/{max_attempts}]Attempt to gracefull kill process with PID {pid} failed : {err} ");
                    current_attempt += 1;
                }
            }
        }

        if process_alive {
            log::warn!("Hard kill process with PID {pid} after {max_attempts} attempts");
            if let Err(err) = child.kill().await {
                log::error!("Unable to force kill the process with PID {pid}. Certainly a zombie process now : {err}")
            }
            if let Err(err) = child.wait().await {
                log::error!("Unable to wait process with PID {pid} to die : {err}")
            }
        }
    }
}

pub async fn create_command(
    token: &str,
    tx: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
    failer: Arc<tokio::sync::Notify>,
) -> eyre::Result<(Arc<tokio::sync::Notify>, pty_process::OwnedWritePty)> {
    let pty = pty_process::Pty::new()?;
    pty.resize(pty_process::Size::new(24, 80))?;

    let command_details = get_command(token)?;

    let mut command = pty_process::Command::new(command_details.command);
    command
        .args(&command_details.args)
        .envs(&command_details.env);

    let mut child = command.spawn(&pty.pts()?)?;

    let pid = child
        .id()
        .map(|x| x.to_string())
        .unwrap_or("Unknown PID".to_string());

    // outgoing psql -> tty [read response] => bytes
    // inbound  tty -> psql [write command] => text

    // reader.next : stdout
    // child.wait : child status
    // aborter.notified : aborter => client décide de couper la connection

    let (outgoing, inbound) = pty.into_split();

    let mut reader = tokio_util::io::ReaderStream::new(outgoing);

    let aborter = Arc::new(tokio::sync::Notify::new());
    let aborter_process = aborter.clone();

    tokio::task::spawn(async move {
        loop {
            tokio::select! {
                data = reader.next() => {
                    if let Some(Ok(data)) = data {
                        if let Err(err) = tx.send(data) {
                            log::warn!("Unable to send data from process to websocket {err}")
                        }
                    }
                },
                exit_status = child.wait() => {
                    format_process_exit_status(exit_status, &pid, failer).await;
                    break
                },
                _ = aborter_process.notified() => {
                    log::info!("Process ask to stop from outside");
                    handle_process_termination(&mut child).await;
                    break
                }
            }
        }
    });

    Ok((aborter, inbound))
}

type Sender = tokio::sync::mpsc::UnboundedSender<bytes::Bytes>;
pub type Aborter = Arc<Mutex<Option<Arc<tokio::sync::Notify>>>>;
type Outgoing = Arc<
    Mutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<tokio_native_tls::TlsStream<tokio::net::TcpStream>>,
            Message,
        >,
    >,
>;
type MaybeMessage = Option<Result<Message, tokio_tungstenite::tungstenite::Error>>;

enum Command {
    Start,
    Quit,
    Other(String),
}

impl From<&String> for Command {
    fn from(value: &String) -> Self {
        match value.as_str() {
            "start" => Command::Start,
            "quit" => Command::Quit,
            x => Other(x.to_string()),
        }
    }
}

async fn handle_message(
    message: MaybeMessage,
    outgoing: Outgoing,
    tx: Sender,
    failer: Arc<tokio::sync::Notify>,
    mut state: State,
) -> eyre::Result<State> {
    if message.is_none() {
        return Ok(State::EndConnection(EndConnection));
    }

    if let Some(Ok(message)) = message {
        let mut ws_sender = outgoing.lock().await;

        match &message {
            Message::Text(text) => {
                let mut message_str = None;
                log::info!("New message {text}");

                let command: Command = text.into();

                match command {
                    Command::Start => {
                        state = state.start();
                        message_str = Some("Enter token:".to_string())
                    }
                    Command::Quit => {
                        state = state.attempt_to_stop_processus();
                        message_str = Some("Process stopped".to_string())
                    }
                    Other(data) => match &mut state {
                        State::Authentication(authentication) => {
                            authentication.set_token(&data);
                            authentication.set_tx(tx.clone(), failer.clone());
                            state = state.attempt_to_run_processus().await;

                            match state {
                                State::Started(_) => {
                                    message_str = Some("Process started".to_string())
                                }
                                State::Failed(_) => {
                                    message_str = Some("Unable to start process".to_string())
                                }
                                _ => {}
                            }
                        }
                        State::Started(started) => {
                            if let Err(err) = started.write(data.as_bytes()).await {
                                log::error!("Unable to send data to PTY : {err}")
                            }
                        }
                        _ => {}
                    },
                }

                if let Some(message_str) = message_str {
                    ws_sender
                        .send(Message::Text(message_str))
                        .await
                        .map_err(|err| eyre!("Unable to send message to stream : {err}"))?;
                    ws_sender
                        .flush()
                        .await
                        .map_err(|err| eyre!("Unable to flush stream : {err}"))?;
                }
            }
            Message::Binary(data) => {
                log::info!("New binary {data:?}")
            }
            Message::Ping(_) => {}
            Message::Pong(_) => {}
            Message::Close(_) => {
                state = state.attempt_to_stop_processus();
                log::info!("Close connection");
            }
            Message::Frame(_) => {}
        };
    }
    Ok(state)
}

async fn handle_connection(
    stream: tokio_native_tls::TlsStream<tokio::net::TcpStream>,
) -> eyre::Result<()> {
    let websocket = tokio_tungstenite::accept_async(stream).await?;

    let (outgoing, mut incoming) = websocket.split();
    let outgoing = Arc::new(Mutex::new(outgoing));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
    let failer = Arc::new(tokio::sync::Notify::new());

    let mut current_state = tokio::sync::OnceCell::new();
    current_state.set(State::default())?;

    loop {
        tokio::select! {
            data = rx.recv() => {
                dbg!(&data);
                if let Some(data) = data {
                    let mut writer = outgoing.lock().await;
                    writer.send(Message::Binary(data.to_vec())).await?;
                }
            }
            _ = failer.notified() => {
                log::warn!("Process exiting");
                let state = current_state.take().ok_or(eyre!("Unable to get State"))?;
                let new_state = state.attempt_to_stop_processus();
                current_state.set(new_state)?;

                let mut writer = outgoing.lock().await;
                writer.send(Message::Text("Process failed".to_string())).await?;

            }
            message = incoming.next() => {

                let state = current_state.take().ok_or(eyre!("Unable to get State"))?;
                let new_state = handle_message(message, outgoing.clone(), tx.clone(), failer.clone(), state).await?;
                //dbg!(&new_state);

                if let State::EndConnection(_) = new_state {
                    break
                }

            current_state.set(new_state)?;

            }
        }
    }

    log::info!("End connection");

    Ok(())
}

async fn server() -> eyre::Result<()> {
    let port = 9001;
    let host = "127.0.0.1";

    let pkcs12_bytes = include_bytes!("../assets/key.p12");
    let identity = native_tls::Identity::from_pkcs12(pkcs12_bytes, "toto")?;

    // create tls acceptor

    let acceptor = tokio_native_tls::TlsAcceptor::from(native_tls::TlsAcceptor::new(identity)?);
    let acceptor = Arc::new(acceptor);

    let server = tokio::net::TcpListener::bind((host, port)).await?;
    log::info!("Listening at {host}:{port}");

    while let Ok((stream, _)) = server.accept().await {
        log::info!("New connection from {:?}", stream.peer_addr());

        let stream = match acceptor.accept(stream).await {
            Ok(stream) => {
                log::debug!("Accept TLS stream");
                stream
            }
            Err(err) => {
                log::error!("An error occurred while init TLS connection {:?}", err);
                continue;
            }
        };

        tokio::task::spawn(handle_connection(stream));
    }

    Ok(())
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    env_logger::init();

    tokio::task::spawn(server()).await??;

    Ok(())
}
