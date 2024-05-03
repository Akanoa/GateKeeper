use actix_web::{get, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use gatekeeper::logic;

#[get("/")]
async fn index() -> impl Responder {
    HttpResponse::Ok().body("Ok")
}

#[get("/ws")]
async fn ws(req: HttpRequest, stream: web::Payload) -> Result<HttpResponse, actix_web::Error> {
    let (response, session, msg_stream) = actix_ws::handle(&req, stream)?;

    tokio::task::spawn_local(async move { logic(msg_stream, session).await });

    Ok(response)
}

async fn http_server() -> eyre::Result<()> {
    let mut builder =
        openssl::ssl::SslAcceptor::mozilla_intermediate(openssl::ssl::SslMethod::tls())?;

    builder.set_private_key_file(
        "./assets/lab.guern.eu+3-key.pem",
        openssl::ssl::SslFiletype::PEM,
    )?;
    builder.set_certificate_file(
        "./assets/lab.guern.eu+3.pem",
        openssl::ssl::SslFiletype::PEM,
    )?;

    HttpServer::new(|| {
        App::new()
            .wrap(actix_web::middleware::Logger::default())
            .service(index)
            .service(ws)
    })
    .bind_openssl("0.0.0.0:3000", builder)?
    .run()
    .await?;

    Ok(())
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    env_logger::init();

    http_server().await?;
    //tokio::task::spawn(server()).await??;

    Ok(())
}
