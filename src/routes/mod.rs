use axum::Router;

use crate::state::AppState;

pub mod administracion;
pub mod aichat;
pub mod backlog;
pub mod catalogos;
pub mod dashboard;
pub mod gestion;
pub mod hub;
pub mod leads;
pub mod misc;
pub mod meetings;
pub mod profile;
pub mod prospecting;
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
        .merge(catalogos::router())
        .merge(prospecting::router())
        .merge(backlog::router())
        .merge(misc::router())
        .merge(gestion::router())
        .merge(administracion::router())
}
