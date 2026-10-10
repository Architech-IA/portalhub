use axum::Router;

use crate::state::AppState;

pub mod administracion;
pub mod auth;
pub mod agentes;
pub mod aichat;
pub mod backlog;
pub mod catalogos;
pub mod central;
pub mod council;
pub mod dashboard;
pub mod ejecutor;
pub mod fases;
pub mod gestion;
pub mod hub;
pub mod leads;
pub mod misc;
pub mod motor;
pub mod orion;
pub mod meetings;
pub mod profile;
pub mod propuestas_docs;
pub mod prospecting;
pub mod realtime;
pub mod prospeccion;
pub mod proyectos;
pub mod triggers;
pub mod solucion_hub;
pub mod soluciones_ia;
pub mod users;
pub mod varios;

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
        .merge(varios::router())
        .merge(orion::router())
        .merge(council::router())
        .merge(prospeccion::router())
        .merge(proyectos::router())
        .merge(agentes::router())
        .merge(triggers::router())
        .merge(soluciones_ia::router())
        .merge(solucion_hub::router())
        .merge(ejecutor::router())
        .merge(fases::router())
        .merge(central::router())
        .merge(propuestas_docs::router())
        .merge(realtime::router())
        .merge(auth::router())
}
