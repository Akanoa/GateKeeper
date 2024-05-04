use actix_web::{get, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use gatekeeper::logic;
use rust_embed_for_web::{EmbedableFile, RustEmbed};
use serde_json::json;

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

#[get("/config")]
async fn configuration() -> impl Responder {
    HttpResponse::Ok().json(json!({
        "reconnect": true,
        "title" : "Mon super titre"
    }))
}

#[get("/{_:.*}")]
async fn dist(path: web::Path<String>) -> impl Responder {
    handle_static(path.as_str())
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
            .service(ws)
            .service(configuration)
            .service(dist)
    })
    .bind_openssl("0.0.0.0:3000", builder)?
    .run()
    .await?;

    Ok(())
}

#[derive(RustEmbed)]
#[folder = "web/dist"]
struct StaticFiles;

fn handle_static(path: &str) -> HttpResponse {
    let mut path = path.trim_start_matches("/");

    if path == "/" {
        path = "index.html"
    }

    match StaticFiles::get(path) {
        Some(content) => HttpResponse::Ok()
            .content_type(mime_guess::from_path(path).first_or_octet_stream().as_ref())
            .body(content.data().to_owned()),
        None => HttpResponse::NotFound().body("404 Not Found"),
    }
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    env_logger::init();

    http_server().await?;
    //tokio::task::spawn(server()).await??;

    Ok(())
}
