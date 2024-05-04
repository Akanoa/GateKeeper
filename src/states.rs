use crate::{create_command, Sender};
use pty_process::OwnedWritePty;
use std::fmt::Debug;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

#[derive(Default, Debug)]
pub struct Init;
#[derive(Default, Debug)]
pub struct Authentication {
    token: Option<String>,
    tx: Option<(Sender, Arc<tokio::sync::Notify>)>,
}
#[derive(Default, Debug)]
pub struct Started {
    aborter: Arc<tokio::sync::Notify>,
    writer: Option<OwnedWritePty>,
}
#[derive(Default, Debug)]
pub struct EndConnection;

#[derive(Default, Debug)]
pub struct Failed;

impl Authentication {
    pub fn set_token(&mut self, token: &str) {
        self.token = Some(token.to_string())
    }
    pub fn set_tx(&mut self, tx: Sender, failer: Arc<tokio::sync::Notify>) {
        self.tx = Some((tx, failer))
    }
}

impl Started {
    pub async fn write(&mut self, data: &[u8]) -> eyre::Result<()> {
        if let Some(ref mut writer) = self.writer {
            writer.write(data).await?;
            // writer.write(b"\r\n").await?;
            writer.flush().await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum State {
    Init(Init),
    Authentication(Authentication),
    Started(Started),
    EndConnection(EndConnection),
    Failed(Failed),
}

impl Default for State {
    fn default() -> Self {
        State::Init(Init)
    }
}

impl State {
    pub fn start(self) -> Self {
        match self {
            State::Init(_) | State::Failed(_) => State::Authentication(Authentication::default()),
            State::Authentication(_) | State::Started(_) => self,
            State::EndConnection(_) => State::Init(Init),
        }
    }

    pub async fn attempt_to_run_processus(self) -> Self {
        match self {
            State::Authentication(authentication) => {
                if let Some(ref token) = authentication.token {
                    if let Some((tx, failer)) = authentication.tx {
                        let process_run_attempt =
                            create_command(token, tx.clone(), failer.clone()).await;

                        if let Ok((aborter, writer)) = process_run_attempt {
                            return State::Started(Started {
                                aborter,
                                writer: Some(writer),
                            });
                        }
                    }
                }

                State::Failed(Failed)
            }
            State::Init(_) | State::Started(_) | State::EndConnection(_) | State::Failed(_) => self,
        }
    }

    pub fn attempt_to_stop_processus(self) -> Self {
        match self {
            State::Started(started) => {
                started.aborter.notify_one();
                State::Init(Init)
            }
            State::Init(_)
            | State::Authentication(_)
            | State::EndConnection(_)
            | State::Failed(_) => self,
        }
    }
}
