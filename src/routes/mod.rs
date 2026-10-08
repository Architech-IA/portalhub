use axum::Router;

use crate::state::AppState;

pub mod aichat;
pub mod dashboard;
pub mod hub;
pub mod leads;
pub mod meetings;
pub mod profile;
pub mod users;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(users::router())
        .merge(profile::router())
        .merge(meetings::router())
        .merge(dashboard::router())
        .merge(leads::router())
        .merge(hub::router())
        .merge(aichat::router())
}
