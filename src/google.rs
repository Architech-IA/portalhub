//! Sincronización de reuniones con Google Calendar — puerto de `src/lib/googleCalendar.ts`.
//! Todo es "fire and forget": un fallo de Google nunca rompe la operación sobre la reunión
//! (igual que en Next, donde las llamadas llevan `.catch(() => {})`).

use serde_json::{json, Value};

use crate::{
    state::AppState,
    util::{exec, fetch_json_opt, fetch_text_opt, B},
};

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const CAL_URL: &str = "https://www.googleapis.com/calendar/v3/calendars/primary/events";

async fn refrescar_token(st: &AppState, user_id: &str) -> Option<String> {
    let refresh = fetch_text_opt(
        &st.pool,
        r#"SELECT "googleRefreshToken" FROM "User" WHERE id = $1"#,
        &[B::T(user_id.to_string())],
    )
    .await
    .ok()
    .flatten()?;

    let res = st
        .http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", st.cfg.google_client_id.clone().unwrap_or_default()),
            ("client_secret", st.cfg.google_client_secret.clone().unwrap_or_default()),
            ("refresh_token", refresh),
            ("grant_type", "refresh_token".to_string()),
        ])
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let data: Value = res.json().await.ok()?;
    let access = data.get("access_token")?.as_str()?.to_string();
    let expira = data.get("expires_in").and_then(|e| e.as_f64());

    let _ = exec(
        &st.pool,
        r#"UPDATE "User" SET "googleAccessToken" = $2,
             "googleTokenExpiry" = CASE WHEN $3::float8 IS NULL THEN NULL
                                        ELSE (NOW() AT TIME ZONE 'UTC') + make_interval(secs => $3::float8) END,
             "updatedAt" = NOW()
           WHERE id = $1"#,
        &[B::T(user_id.to_string()), B::T(access.clone()), expira.map(B::F).unwrap_or(B::OT(None))],
    )
    .await;
    Some(access)
}

/// `getValidAccessToken`: usa el token guardado si todavía no venció; si no, lo renueva.
async fn token_valido(st: &AppState, user_id: &str) -> Option<String> {
    let fila = fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'access', "googleAccessToken",
             'vigente', ("googleTokenExpiry" IS NOT NULL AND (NOW() AT TIME ZONE 'UTC') < "googleTokenExpiry"))
           FROM "User" WHERE id = $1"#,
        &[B::T(user_id.to_string())],
    )
    .await
    .ok()
    .flatten()?;
    let access = fila.get("access")?.as_str()?.to_string();
    if fila.get("vigente").and_then(|v| v.as_bool()) == Some(true) {
        return Some(access);
    }
    refrescar_token(st, user_id).await
}

async fn leer_reunion(st: &AppState, meeting_id: &str) -> Option<Value> {
    fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'title', title, 'description', description, 'date', date,
             'endEff', COALESCE("endDate", date + interval '1 hour'),
             'location', location, 'attendees', attendees, 'link', link, 'status', status,
             'googleEventId', "googleEventId", 'userId', "userId")
           FROM "Meeting" WHERE id = $1"#,
        &[B::T(meeting_id.to_string())],
    )
    .await
    .ok()
    .flatten()
}

fn texto<'a>(m: &'a Value, k: &str) -> Option<&'a str> {
    m.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn cuerpo_evento(m: &Value, cancelada: bool) -> Value {
    let invitados: Vec<Value> = texto(m, "attendees")
        .map(|a| {
            a.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(|email| json!({ "email": email }))
                .collect()
        })
        .unwrap_or_default();

    let mut cuerpo = json!({
        "summary": m.get("title").cloned().unwrap_or(Value::Null),
        "start": { "dateTime": m.get("date"), "timeZone": "America/Bogota" },
        "end": { "dateTime": m.get("endEff"), "timeZone": "America/Bogota" },
        "attendees": invitados,
    });

    let mut descripcion: Option<String> = texto(m, "description").map(|d| d.to_string());
    if let Some(l) = texto(m, "location") {
        cuerpo["location"] = json!(l);
    }
    if let Some(link) = texto(m, "link") {
        let previo = descripcion.as_ref().map(|d| format!("{d}\n\n")).unwrap_or_default();
        descripcion = Some(format!("{previo}Enlace: {link}"));
    }
    if cancelada {
        descripcion = Some(format!("[CANCELADA]\n{}", descripcion.unwrap_or_default()));
    }
    if let Some(d) = descripcion {
        cuerpo["description"] = json!(d);
    }
    cuerpo
}

/// `createCalendarEvent` — se lanza en segundo plano.
pub fn crear(st: AppState, meeting_id: String) {
    tokio::spawn(async move {
        let Some(m) = leer_reunion(&st, &meeting_id).await else { return };
        let Some(uid) = texto(&m, "userId").map(|u| u.to_string()) else { return };
        let Some(token) = token_valido(&st, &uid).await else { return };

        let res = st
            .http
            .post(format!("{CAL_URL}?sendUpdates=all"))
            .bearer_auth(&token)
            .json(&cuerpo_evento(&m, false))
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => {
                if let Ok(data) = r.json::<Value>().await {
                    if let Some(id) = data.get("id").and_then(|i| i.as_str()) {
                        let _ = exec(
                            &st.pool,
                            r#"UPDATE "Meeting" SET "googleEventId" = $2, "updatedAt" = NOW() WHERE id = $1"#,
                            &[B::T(meeting_id), B::T(id.to_string())],
                        )
                        .await;
                    }
                }
            }
            Ok(r) => tracing::error!("Google Calendar create error: {}", r.text().await.unwrap_or_default()),
            Err(e) => tracing::error!("Google Calendar create exception: {e}"),
        }
    });
}

/// `updateCalendarEvent` — se lanza en segundo plano.
pub fn actualizar(st: AppState, meeting_id: String) {
    tokio::spawn(async move {
        let Some(m) = leer_reunion(&st, &meeting_id).await else { return };
        let Some(evento) = texto(&m, "googleEventId").map(|e| e.to_string()) else { return };
        let Some(uid) = texto(&m, "userId").map(|u| u.to_string()) else { return };
        let Some(token) = token_valido(&st, &uid).await else { return };
        let cancelada = texto(&m, "status") == Some("CANCELLED");

        if let Err(e) = st
            .http
            .patch(format!("{CAL_URL}/{evento}?sendUpdates=all"))
            .bearer_auth(&token)
            .json(&cuerpo_evento(&m, cancelada))
            .send()
            .await
        {
            tracing::error!("Google Calendar update exception: {e}");
        }
    });
}

/// `deleteCalendarEvent` — recibe el id del evento ya leído (antes de borrar la reunión).
pub fn eliminar(st: AppState, user_id: String, event_id: String) {
    tokio::spawn(async move {
        let Some(token) = token_valido(&st, &user_id).await else { return };
        if let Err(e) = st
            .http
            .delete(format!("{CAL_URL}/{event_id}?sendUpdates=all"))
            .bearer_auth(&token)
            .send()
            .await
        {
            tracing::error!("Google Calendar delete exception: {e}");
        }
    });
}
