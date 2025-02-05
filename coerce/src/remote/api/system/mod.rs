use crate::remote::api::Routes;

use crate::remote::system::RemoteActorSystem;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};

pub struct SystemApi {
    system: RemoteActorSystem,
}

impl SystemApi {
    pub fn new(system: RemoteActorSystem) -> Self {
        Self { system }
    }
}

impl Routes for SystemApi {
    fn routes(&self, router: Router) -> Router {
        router.route("/system/stats", {
            let system = self.system.clone();
            get(move || get_stats(system))
        })
    }
}

#[derive(Serialize, Deserialize)]
pub struct SystemStats {
    inflight_remote_requests: usize,
    total_tracked_actors: usize,
}

async fn get_stats(system: RemoteActorSystem) -> impl IntoResponse {
    Json(SystemStats {
        inflight_remote_requests: system.inflight_remote_request_count(),
        total_tracked_actors: system
            .actor_system()
            .scheduler()
            .exec(|s| s.actors.len())
            .await
            .unwrap(),
    })
}
