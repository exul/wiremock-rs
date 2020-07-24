use crate::active_mock::ActiveMock;
use crate::{HttpRequest, Mock, Request, WebsocketRequest};
use async_tungstenite::tungstenite;
use bastion::prelude::*;
use futures_timer::Delay;
use http_types::{Response, StatusCode};
use log::{debug, warn};
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct MockActor {
    pub actor_ref: ChildRef,
}

#[derive(Clone, Debug)]
struct Reset {}

#[derive(Clone, Debug)]
struct Verify {}

impl MockActor {
    /// Start an instance of our MockActor and return a reference to it.
    pub(crate) fn start<R: Into<Request> + Sized + std::fmt::Debug>() -> MockActor {
        let mock_actors = Bastion::children(|children: Children| {
            children.with_exec(move |ctx: BastionContext| async move {
                let mut mocks: Vec<ActiveMock<HttpRequest>> = vec![];
                let mut ws_mocks: Vec<ActiveMock<WebsocketRequest>> = vec![];
                loop {
                    msg! { ctx.recv().await?,
                        _reset: Reset =!> {
                            debug!("Dropping all mocks.");
                            mocks = vec![];
                            answer!(ctx, "Reset.").unwrap();
                        };
                        _verify: Verify =!> {
                            debug!("Verifying expectations for all mounted mocks.");
                            let verified = mocks.iter().all(|m| m.verify());
                            answer!(ctx, verified).unwrap();
                        };
                        mock: Mock<HttpRequest> =!> {
                            debug!("Registering http mock.");
                            mocks.push(ActiveMock::new(mock));
                            answer!(ctx, "Registered.").unwrap();
                        };
                        mock: Mock<WebsocketRequest> =!> {
                            debug!("Registering http mock.");
                            ws_mocks.push(ActiveMock::new(mock));
                            answer!(ctx, "Registered.").unwrap();
                        };
                        request: http_types::Request =!> {
                            debug!("Handling request.");
                            let request = Request::from(request).await;

                            let mut response: Option<Response> = None;
                            let mut delay: Option<Duration> = None;
                            for mock in &mut mocks {
                                if mock.matches(&request) {
                                    response = Some(mock.response());
                                    delay = mock.delay().to_owned();
                                    break;
                                }
                            }
                            if let Some(response) = response {
                                if let Some(delay) = delay {
                                    Delay::new(delay).await;
                                }
                                answer!(ctx, response).unwrap();
                            } else {
                                debug!("Got unexpected request:\n{}", request);
                                let res = Response::new(StatusCode::NotFound);
                                answer!(ctx, res).unwrap();
                            }
                        };
                        request: tungstenite::Message =!> {
                            debug!("Handling websocket request");
                            let res = tungstenite::Message::Text("Hello".to_string());
                            answer!(ctx, res).unwrap();
                        };
                        _: _ => {
                            warn!("Received a message I was not listening for.");
                        };
                    }
                }
            })
        })
        .expect("Couldn't create the mock actor.");
        // We actually started only one actor
        let mock_actor = mock_actors.elems()[0].clone();
        MockActor {
            actor_ref: mock_actor,
        }
    }

    pub(crate) async fn register<R: Into<Request> + Sized + std::fmt::Debug>(&self, mock: Mock<R>) {
        self.actor_ref.ask_anonymously(mock).unwrap().await.unwrap();
    }

    pub(crate) async fn reset(&self) {
        self.actor_ref
            .ask_anonymously(Reset {})
            .unwrap()
            .await
            .unwrap();
    }

    pub(crate) async fn verify(&self) -> bool {
        let answer = self.actor_ref.ask_anonymously(Verify {}).unwrap();
        let response = msg! { answer.await.expect("Couldn't receive the answer."),
            outcome: bool => outcome;
            _: _ => false;
        };
        response
    }
}
