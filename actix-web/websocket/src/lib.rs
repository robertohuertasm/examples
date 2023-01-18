use actix::prelude::*;
use actix_files::NamedFile;
use actix_web::{
    web::{self, ServiceConfig},
    HttpRequest, HttpResponse, Responder,
};
use actix_web_actors::ws;
use chrono::{DateTime, Utc};
use serde::Serialize;
use shuttle_service::ShuttleActixWeb;
use std::{collections::HashSet, time::Duration};

const PAUSE_SECS: u64 = 15;
const STATUS_URI: &str = "https://api.shuttle.rs";

// actor used to check the api and send messages to all the ws actors
#[derive(Default)]
struct ApiCheckerActor {
    addresses: HashSet<Addr<WsActor>>,
}

// message sent from the ws actors when they connect or disconnect
// used to keep track of the number of connected clients
#[derive(actix::Message, Clone, Debug)]
#[rtype(result = "()")]
enum ApiCheckerMessage {
    Connected(Addr<WsActor>),
    Disconnected(Addr<WsActor>),
}

#[derive(Serialize, actix::Message, Default, Clone, Debug)]
#[rtype(result = "()")]
struct ApiCheckerResponse {
    clients_count: usize,
    date_time: DateTime<Utc>,
    is_up: bool,
}

impl Actor for ApiCheckerActor {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        let duration = Duration::from_secs(PAUSE_SECS);
        let client = reqwest::Client::default();

        ctx.run_interval(duration, move |_, ctx| {
            let addr = ctx.address();
            let client = client.clone();
            let _fut = async move {
                let is_up = client.get(STATUS_URI).send().await;
                let is_up = is_up.is_ok();

                let response = ApiCheckerResponse {
                    clients_count: 0,
                    date_time: Utc::now(),
                    is_up,
                };

                addr.do_send(response);
            };

            // fut.into_actor(self).spawn(ctx);
        });
    }
}

impl Handler<ApiCheckerResponse> for ApiCheckerActor {
    type Result = ();

    fn handle(&mut self, mut msg: ApiCheckerResponse, _ctx: &mut Self::Context) {
        tracing::info!("API Checker: {msg:?}");
        msg.clients_count = self.addresses.len();
        for addr in self.addresses.iter() {
            addr.do_send(msg.clone());
        }
    }
}

impl Handler<ApiCheckerMessage> for ApiCheckerActor {
    type Result = ();

    fn handle(&mut self, msg: ApiCheckerMessage, _ctx: &mut Self::Context) {
        match msg {
            ApiCheckerMessage::Connected(addr) => {
                self.addresses.insert(addr);
                // TODO: send the current status to the new client
            }
            ApiCheckerMessage::Disconnected(addr) => {
                self.addresses.remove(&addr);
                // TODO: send the current status to the remaining clients
            }
        }
    }
}

struct WsActor {
    api_checker_addr: Addr<ApiCheckerActor>,
}

impl WsActor {
    fn new(api_checker_addr: Addr<ApiCheckerActor>) -> Self {
        Self { api_checker_addr }
    }
}

impl Actor for WsActor {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        let addr = ctx.address();
        self.api_checker_addr
            .do_send(ApiCheckerMessage::Connected(addr));
    }
}

impl Handler<ApiCheckerResponse> for WsActor {
    type Result = ();

    fn handle(&mut self, msg: ApiCheckerResponse, ctx: &mut Self::Context) {
        let msg = serde_json::to_string(&msg).unwrap();
        ctx.text(msg);
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsActor {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        tracing::info!("WS: {msg:?}");
        match msg {
            Ok(ws::Message::Text(text)) => ctx.text(text),
            Ok(ws::Message::Close(reason)) => {
                self.api_checker_addr
                    .do_send(ApiCheckerMessage::Disconnected(ctx.address()));
                ctx.close(reason)
            }
            _ => (),
        }
    }
}

async fn websocket(
    req: HttpRequest,
    stream: web::Payload,
    app_state: web::Data<Addr<ApiCheckerActor>>,
) -> actix_web::Result<HttpResponse> {
    let addr = app_state.get_ref().clone();
    let ws_actor = WsActor::new(addr);
    let response = ws::start(ws_actor, &req, stream);
    tracing::info!("New WS: {response:?}");
    response
}

async fn index() -> impl Responder {
    NamedFile::open_async("./static/index.html")
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))
}

#[shuttle_service::main]
async fn actix_web(
) -> ShuttleActixWeb<impl FnOnce(&mut ServiceConfig) + Sync + Send + Clone + 'static> {
    // let's create an actor to continuously check the status of the shuttle
    let api_checker_addr = ApiCheckerActor::default().start();
    let app_state = web::Data::new(api_checker_addr);

    Ok(move |cfg: &mut ServiceConfig| {
        cfg.service(web::resource("/").route(web::get().to(index)))
            .service(
                web::resource("/ws")
                    .app_data(app_state)
                    .route(web::get().to(websocket)),
            );
    })

    // let api_checker_addr = ApiCheckerActor::default().start();
    // then pass the actor address to the websocket handler
}
