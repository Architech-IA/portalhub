//! Prospector: búsqueda en Google Places, autocompletar de ciudades, conversión a clientes/leads y
//! estadísticas de uso — MASD PHUB-0001-0011. Paridad con `src/app/api/prospecting/{search,
//! autocomplete,convert,stats}/route.ts` (la tabla de resultados ya estaba en `prospecting.rs`).

use std::time::Duration;

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, new_id, s, B},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/prospecting/search", post(buscar))
        .route("/api/prospecting/autocomplete", get(autocompletar))
        .route("/api/prospecting/convert", post(convertir))
        .route("/api/prospecting/stats", get(estadisticas))
}

const PLACES_URL: &str = "https://places.googleapis.com/v1/places:searchText";
const FIELD_MASK: &str =
    "places.displayName,places.formattedAddress,places.internationalPhoneNumber,places.websiteUri,places.rating,places.userRatingCount,places.businessStatus,places.types,places.location,places.id";

/// Coordenadas de ciudades colombianas (el nombre se normaliza a minúsculas).
const CIUDADES: &[(&str, f64, f64)] = &[
    ("bogotá", 4.7110, -74.0721), ("bogota", 4.7110, -74.0721), ("medellín", 6.2442, -75.5812), ("medellin", 6.2442, -75.5812),
    ("cali", 3.4516, -76.5320), ("barranquilla", 10.9685, -74.7813), ("cartagena", 10.3910, -75.4794), ("cúcuta", 7.8939, -72.5078),
    ("cucuta", 7.8939, -72.5078), ("bucaramanga", 7.1198, -73.1227), ("pereira", 4.8133, -75.6961), ("santa marta", 11.2408, -74.2110),
    ("ibagué", 4.4389, -75.2322), ("ibague", 4.4389, -75.2322), ("pasto", 1.2136, -77.2811), ("manizales", 5.0703, -75.5138),
    ("neiva", 2.9273, -75.2819), ("villavicencio", 4.1420, -73.6266), ("armenia", 4.5339, -75.6811), ("valledupar", 10.4631, -73.2532),
    ("montería", 8.7479, -75.8814), ("monteria", 8.7479, -75.8814), ("sincelejo", 9.3047, -75.3978), ("popayán", 2.4448, -76.6147),
    ("popayan", 2.4448, -76.6147), ("tunja", 5.5353, -73.3678), ("florencia", 1.6144, -75.6062), ("riohacha", 11.5444, -72.9072),
    ("quibdó", 5.6948, -76.6612), ("quibdo", 5.6948, -76.6612), ("mocoa", 1.1522, -76.6483), ("arauca", 7.0900, -70.7617),
    ("yopal", 5.3378, -72.3959), ("san josé del guaviare", 2.5706, -72.6406), ("leticia", -4.2153, -69.9406), ("inírida", 3.8653, -67.9239),
    ("mitú", 1.2536, -70.2351), ("puerto carreño", 6.1892, -67.4850), ("bello", 6.3367, -75.5578), ("itagüí", 6.1844, -75.5993),
    ("itagui", 6.1844, -75.5993), ("envigado", 6.1743, -75.5913), ("rionegro", 6.1547, -75.3730), ("sabaneta", 6.1508, -75.6172),
    ("copacabana", 6.3489, -75.5072), ("apartadó", 7.8789, -76.6294), ("turbo", 8.1004, -76.7303), ("caucasia", 7.9883, -75.1958),
    ("caldas", 6.0956, -75.6336), ("palmira", 3.5394, -76.3036), ("buenaventura", 3.8801, -77.0311), ("tuluá", 4.0845, -76.2013),
    ("tulua", 4.0845, -76.2013), ("buga", 3.8994, -76.2980), ("cartago", 4.7483, -75.9122), ("jamundí", 3.2618, -76.5391),
    ("soacha", 4.5792, -74.2175), ("zipaquirá", 5.0228, -74.0056), ("facatativá", 4.8145, -74.3569), ("fusagasugá", 4.3375, -74.3644),
    ("girardot", 4.3031, -74.8027), ("chía", 4.8596, -74.0596), ("mosquera", 4.7064, -74.2303), ("madrid", 4.7348, -74.2670),
    ("floridablanca", 7.0644, -73.0961), ("piedecuesta", 6.9930, -73.0544), ("barrancabermeja", 7.0650, -73.8538), ("san gil", 6.5578, -73.1350),
    ("soledad", 10.9167, -74.7667), ("malambo", 10.8628, -74.7775), ("magangué", 9.2406, -74.7547), ("magangue", 9.2406, -74.7547),
    ("sogamoso", 5.7193, -72.9294), ("duitama", 5.8233, -73.0270), ("espinal", 4.1528, -74.8847), ("honda", 5.2022, -74.7414),
    ("tumaco", 1.7990, -78.7619), ("lorica", 9.2306, -75.8139), ("cereté", 8.8903, -75.7923), ("dosquebradas", 4.8392, -75.6628),
    ("santander de quilichao", 3.0087, -76.4836), ("aguachica", 8.3108, -73.6167), ("pitalito", 1.8550, -76.0481), ("garzón", 2.1992, -75.6267),
    ("acacías", 3.9886, -73.7601), ("maicao", 11.3722, -72.2437),
];

/// `Number(x)` de un valor del cuerpo (número o texto numérico).
fn num(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(t) => t.trim().parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

fn respuesta_error(status: u16, msg: impl Into<String>) -> ApiError {
    ApiError::new(axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR), msg)
}

async fn buscar(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autenticado"))?;
    let Some(clave) = st.cfg.google_places_api_key.clone() else {
        return Err(ApiError::internal("API key no configurada"));
    };
    let ciudad = s(&body, "city").filter(|c| !c.is_empty());
    let categoria = s(&body, "category").filter(|c| !c.is_empty());
    let tiene_coords = body.get("lat").is_some() && body.get("lng").is_some();
    let Some(categoria) = categoria else { return Err(ApiError::bad_request("Categoría es requerida")) };
    if ciudad.is_none() && !tiene_coords {
        return Err(ApiError::bad_request("Ubicación es requerida"));
    }
    let radio = body.get("radius").map(num).filter(|r| r.is_finite()).unwrap_or(5000.0);
    let max_resultados = body.get("maxResults").map(num).filter(|r| r.is_finite()).unwrap_or(20.0).min(50.0);

    let coords: Option<(f64, f64)> = if tiene_coords {
        Some((num(&body["lat"]), num(&body["lng"])))
    } else {
        ciudad.as_deref().and_then(|c| {
            let n = c.to_lowercase();
            CIUDADES.iter().find(|(k, _, _)| *k == n.trim()).map(|(_, la, lo)| (*la, *lo))
        })
    };
    let etiqueta = ciudad.clone().unwrap_or_else(|| format!("{:.4}, {:.4}", num(&body["lat"]), num(&body["lng"])));
    let consulta = format!("{categoria} en {etiqueta}, Colombia");
    let mut cuerpo = json!({ "textQuery": consulta, "languageCode": "es", "maxResultCount": max_resultados });
    if let Some((la, lo)) = coords {
        cuerpo["locationBias"] = json!({ "circle": { "center": { "latitude": la, "longitude": lo }, "radius": radio } });
    }
    let r = st
        .http
        .post(PLACES_URL)
        .header("X-Goog-Api-Key", clave)
        .header("X-Goog-FieldMask", FIELD_MASK)
        .timeout(Duration::from_secs(60))
        .json(&cuerpo)
        .send()
        .await
        .map_err(|_| ApiError::internal("Error al conectar con Google Places"))?;
    let status = r.status();
    let datos: Value = r.json().await.map_err(|_| ApiError::internal("Error al conectar con Google Places"))?;
    if !status.is_success() {
        return Err(respuesta_error(status.as_u16(), datos.pointer("/error/message").and_then(|m| m.as_str()).unwrap_or("Error de Google Places")));
    }
    let lugares: Vec<Value> = datos["places"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|p| {
            json!({
                "placeId": p["id"].as_str().unwrap_or(""),
                "name": p.pointer("/displayName/text").and_then(|x| x.as_str()).unwrap_or(""),
                "address": p["formattedAddress"].as_str().unwrap_or(""),
                "phone": p["internationalPhoneNumber"].as_str().unwrap_or(""),
                "website": p["websiteUri"].as_str().unwrap_or(""),
                "rating": p["rating"].clone(),
                "totalRatings": p["userRatingCount"].as_i64().unwrap_or(0),
                "status": p["businessStatus"].as_str().unwrap_or("OPERATIONAL"),
                "types": p.get("types").cloned().filter(|t| t.is_array()).unwrap_or_else(|| json!([])),
                "lat": p.pointer("/location/latitude").cloned().unwrap_or(Value::Null),
                "lng": p.pointer("/location/longitude").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    // Registro de uso (no bloquea si falla).
    let _ = exec(
        &st.pool,
        r#"INSERT INTO "ProspectingLog" (id, "userId", "userName", city, category, radius, "resultsCount", "fromMap", "createdAt")
           VALUES ($1, $2, $3, $4, $5, $6::float8::int, $7, $8, NOW())"#,
        &[
            B::T(new_id()),
            B::T(if sesion.id.is_empty() { "unknown".into() } else { sesion.id.clone() }),
            B::T(if !sesion.name.is_empty() { sesion.name.clone() } else if !sesion.email.is_empty() { sesion.email.clone() } else { "unknown".into() }),
            B::T(etiqueta),
            B::T(categoria),
            B::F(radio),
            B::I(lugares.len() as i64),
            B::Bo(tiene_coords),
        ],
    )
    .await;
    Ok(Json(json!({ "places": lugares, "total": lugares.len(), "query": consulta })))
}

async fn autocompletar(State(st): State<AppState>, sesion: Session, axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> ApiResult<Json<Value>> {
    sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autenticado"))?;
    let texto = q.get("q").cloned().unwrap_or_default();
    if texto.chars().count() < 2 {
        return Ok(Json(json!({ "suggestions": [] })));
    }
    let Some(clave) = st.cfg.google_places_api_key.clone() else { return Ok(Json(json!({ "suggestions": [] }))) };
    let r = st
        .http
        .post("https://places.googleapis.com/v1/places:autocomplete")
        .header("X-Goog-Api-Key", clave)
        .timeout(Duration::from_secs(30))
        .json(&json!({
            "input": texto, "includedRegionCodes": ["CO"], "languageCode": "es",
            "includedPrimaryTypes": ["locality", "sublocality", "administrative_area_level_2", "administrative_area_level_1"],
        }))
        .send()
        .await;
    let Ok(r) = r else { return Ok(Json(json!({ "suggestions": [] }))) };
    let Ok(d) = r.json::<Value>().await else { return Ok(Json(json!({ "suggestions": [] }))) };
    let sugerencias: Vec<Value> = d["suggestions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|x| {
            json!({
                "placeId": x.pointer("/placePrediction/placeId").and_then(|v| v.as_str()).unwrap_or(""),
                "mainText": x.pointer("/placePrediction/structuredFormat/mainText/text").and_then(|v| v.as_str()).unwrap_or(""),
                "secondaryText": x.pointer("/placePrediction/structuredFormat/secondaryText/text").and_then(|v| v.as_str()).unwrap_or(""),
            })
        })
        .filter(|x| !x["mainText"].as_str().unwrap_or("").is_empty())
        .collect();
    Ok(Json(json!({ "suggestions": sugerencias })))
}

/// "restaurant_bar" → "Restaurant Bar".
fn industria_de(tipos: &[String]) -> String {
    tipos
        .iter()
        .find(|t| !["establishment", "point_of_interest", "food"].contains(&t.as_str()))
        .map(|t| {
            t.replace('_', " ")
                .split(' ')
                .map(|p| {
                    let mut c = p.chars();
                    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| "Sin clasificar".into())
}

async fn convertir(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let usuario = sesion.require_user().map_err(|_| ApiError::unauthorized_msg("No autenticado"))?.to_string();
    let lugares = body.get("places").and_then(|p| p.as_array()).filter(|p| !p.is_empty()).ok_or_else(|| ApiError::bad_request("No se enviaron prospectos"))?;
    let mut creados = 0;
    let mut omitidos: Vec<String> = vec![];
    for lugar in lugares {
        let nombre = s(lugar, "name").unwrap_or_default();
        // Si ya existe un Cliente con ese nombre y ya tiene un Lead, se salta.
        let cliente = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(c) FROM "Cliente" c WHERE lower(c.nombre) = lower($1) LIMIT 1"#, &[B::T(nombre.clone())]).await?;
        let cliente = match cliente {
            Some(c) => {
                let existe = fetch_json_opt(&st.pool, r#"SELECT to_jsonb(l) FROM "Lead" l WHERE l."clienteId" = $1 LIMIT 1"#, &[B::T(c["id"].as_str().unwrap_or("").into())]).await?;
                if existe.is_some() {
                    omitidos.push(nombre);
                    continue;
                }
                c
            }
            None => {
                let tipos: Vec<String> = lugar["types"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                let web = s(lugar, "website").filter(|w| !w.is_empty());
                fetch_json(
                    &st.pool,
                    r#"WITH c AS (INSERT INTO "Cliente" (id, nombre, industria, contacto, email, pais, estado, "valorTotal", "createdAt", "updatedAt")
                         VALUES ($1, $2, $3, '', $4, 'Colombia', 'Activo', 0, NOW(), NOW()) RETURNING *) SELECT to_jsonb(c) FROM c"#,
                    &[B::T(new_id()), B::T(nombre.clone()), B::T(industria_de(&tipos)), B::T(web.map(|w| format!("web: {w}")).unwrap_or_default())],
                )
                .await?
            }
        };
        let lead_nuevo = new_id();
        exec(
            &st.pool,
            r#"INSERT INTO "Lead" (id, "companyName", "contactName", email, phone, status, source, "estimatedValue", "userId", "clienteId", "createdAt", "updatedAt")
               VALUES ($1, $2, '', $3, $4, 'NEW', 'Prospecting', 0, $5, $6, NOW(), NOW())"#,
            &[
                B::T(lead_nuevo.clone()),
                B::T(nombre),
                B::T(cliente["email"].as_str().unwrap_or("").into()),
                B::OT(s(lugar, "phone").filter(|p| !p.is_empty())),
                B::T(usuario.clone()),
                B::T(cliente["id"].as_str().unwrap_or("").into()),
            ],
        )
        .await?;
        if let Err(e) = crate::routes::fases::proyecto_para_lead(&st, &lead_nuevo, &usuario, &sesion.name).await {
            tracing::error!("iniciar el motor del lead convertido: {}", e.1);
        }
        creados += 1;
    }
    Ok(Json(json!({ "created": creados, "skipped": omitidos.len(), "skippedNames": omitidos })))
}

async fn estadisticas(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let d = fetch_json(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'logs', COALESCE((SELECT jsonb_agg(jsonb_build_object('id', id, 'userName', "userName", 'city', city, 'category', category, 'results', "resultsCount",
                              'fromMap', "fromMap", 'createdAt', "createdAt", 'dia', to_char("createdAt", 'YYYY-MM-DD'), 'mes', to_char("createdAt", 'YYYY-MM'))
                              ORDER BY "createdAt" DESC) FROM "ProspectingLog"), '[]'::jsonb),
             'leads', (SELECT COUNT(*) FROM "Lead" WHERE source = 'GOOGLE_PLACES'),
             'hoy', to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD'),
             'mesActual', to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM'),
             'dias30', COALESCE((SELECT jsonb_agg(to_char((now() AT TIME ZONE 'UTC')::date - g, 'YYYY-MM-DD') ORDER BY g DESC) FROM generate_series(0, 29) g), '[]'::jsonb))"#,
        &[],
    )
    .await?;
    let logs = d["logs"].as_array().cloned().unwrap_or_default();
    let total = logs.len();
    let total_resultados: i64 = logs.iter().map(|l| l["results"].as_i64().unwrap_or(0)).sum();
    let redondear2 = |x: f64| (x * 100.0).round() / 100.0;
    let costo = redondear2(total as f64 * 0.032);
    let este_mes: Vec<&Value> = logs.iter().filter(|l| l["mes"] == d["mesActual"]).collect();
    let costo_mes = redondear2(este_mes.len() as f64 * 0.032);

    fn top(logs: &[Value], k: &str, etiqueta: &str, limite: Option<usize>) -> Vec<Value> {
        let mut orden: Vec<(String, i64)> = vec![];
        for l in logs {
            let c = l[k].as_str().unwrap_or("").to_string();
            match orden.iter_mut().find(|(n, _)| *n == c) {
                Some(e) => e.1 += 1,
                None => orden.push((c, 1)),
            }
        }
        orden.sort_by(|a, b| b.1.cmp(&a.1));
        orden.into_iter().take(limite.unwrap_or(usize::MAX)).map(|(n, c)| json!({ etiqueta: n, "count": c })).collect()
    }
    // Corrección deliberada: en Next "últimos 30 días" comparaba el texto de `Date.toString()` con
    // "YYYY-MM-DD" y siempre daba 0; acá se cuenta por día de verdad.
    let ultimos: Vec<Value> = d["dias30"].as_array().cloned().unwrap_or_default().iter().rev().map(|dia| {
        json!({ "date": dia, "count": logs.iter().filter(|l| l["dia"] == *dia).count() })
    }).collect();
    let recientes: Vec<Value> = logs
        .iter()
        .take(10)
        .map(|l| json!({ "id": l["id"], "userName": l["userName"], "city": l["city"], "category": l["category"], "results": l["results"], "fromMap": l["fromMap"], "createdAt": l["createdAt"] }))
        .collect();
    Ok(Json(json!({
        "total": total,
        "thisMonth": este_mes.len(),
        "totalResults": total_resultados,
        "costUSD": costo,
        "thisMonthCost": costo_mes,
        "creditoRestante": redondear2(200.0 - costo),
        "leadsCreated": d["leads"],
        "fromMap": logs.iter().filter(|l| l["fromMap"] == true).count(),
        "topCategories": top(&logs, "category", "category", Some(5)),
        "topCities": top(&logs, "city", "city", Some(5)),
        "byUser": top(&logs, "userName", "name", None),
        "last30": ultimos,
        "recentLogs": recientes,
    })))
}
