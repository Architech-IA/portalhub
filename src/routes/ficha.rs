//! Ficha del cliente dentro del proyecto — MASD-0024. La empresa vive en `Cliente` (la misma tabla que ya usaban los
//! leads, ahora con razón social, NIT, ciudad…); sus personas por papel en `ClienteContacto` (PRINCIPAL, DECISOR,
//! REPRESENTANTE_LEGAL, FIRMANTE) y lo propio de la oportunidad —empresa elegida, fuente, presupuesto, responsable— en
//! `ProyectoFicha`. «Qué quiere resolver» y el valor son los de la Solución (`descripcion`, `valorEstimado`).
//!
//! La ficha está «completa» cuando tiene los 11 datos que pide la fase 1; los valores de relleno («Sin especificar»,
//! `unknow@unknow.com`…) no cuentan. La fase comprueba esto sola (verificación `ficha_cliente`) y las fases de
//! contrato y aceptación piden además que el papel correspondiente esté identificado (`ficha_contacto`).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{exec, fetch_json, fetch_json_opt, fetch_text_opt, new_id, B},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/proyectos/{id}/cliente", get(obtener).put(guardar)).route("/api/fases/clientes", get(buscar))
}

const ROLES: [(&str, &str); 4] = [("principal", "PRINCIPAL"), ("decisor", "DECISOR"), ("representante", "REPRESENTANTE_LEGAL"), ("firmante", "FIRMANTE")];

// ── Validaciones puras ───────────────────────────────────────────────────────────────────────
fn minus(t: &str) -> String {
    t.trim().to_lowercase()
}

/// Texto vacío o de relleno: no cuenta como dato.
pub fn es_relleno(t: &str) -> bool {
    let n = minus(t);
    n.is_empty() || n.contains("unknow") || ["sin especificar", "sin dato", "n/a", "na", "ninguno", "ninguna", "pendiente", "por definir", "xxx", "-", "."].contains(&n.as_str())
}

pub fn correo_valido(c: &str) -> bool {
    let c = minus(c);
    if es_relleno(&c) || c.contains(' ') {
        return false;
    }
    let Some((local, dominio)) = c.split_once('@') else { return false };
    !local.is_empty()
        && dominio.contains('.')
        && !dominio.starts_with('.')
        && !dominio.ends_with('.')
        && !["example.com", "test.com", "correo.com", "email.com", "mail.com"].contains(&dominio)
        && !dominio.ends_with(".example")
        && !["test", "correo", "xxx", "prueba"].contains(&local)
}

pub fn telefono_valido(t: &str) -> bool {
    let d = t.chars().filter(|c| c.is_ascii_digit()).count();
    (7..=15).contains(&d) && t.chars().filter(|c| c.is_ascii_digit()).collect::<String>().chars().collect::<std::collections::HashSet<_>>().len() > 2
}

/// NIT solo con dígitos (con o sin dígito de verificación, con o sin puntos y guion): 8 a 11 dígitos.
pub fn nit_normalizado(t: &str) -> Option<String> {
    let d: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    (8..=11).contains(&d.len()).then_some(d)
}

fn txt(v: &Value) -> &str {
    v.as_str().map(str::trim).unwrap_or_default()
}

fn opt(v: &Value) -> Option<String> {
    let t = txt(v);
    (!t.is_empty()).then(|| t.to_string())
}

// ── Un proyecto nuevo trae su ficha desde el lead ───────────────────────────────────────────
/// Si el proyecto nació de un lead y todavía no tiene ficha, la arma con lo que el lead ya sabe (empresa, contacto, fuente, responsable). Es idempotente.
async fn sembrar_desde_lead(st: &AppState, sol: &str) {
    let _ = exec(
        &st.pool,
        r#"INSERT INTO "ProyectoFicha" ("solucionId", "clienteId", fuente, "responsableId", "responsableNombre")
             SELECT s.id, l."clienteId", l.source, l."userId", u.name FROM "Solucion" s JOIN "Lead" l ON l.id = s."leadId" LEFT JOIN "User" u ON u.id = l."userId" WHERE s.id = $1
             ON CONFLICT ("solucionId") DO NOTHING"#,
        &[B::T(sol.into())],
    )
    .await;
    let _ = exec(
        &st.pool,
        r#"INSERT INTO "ClienteContacto" (id, "clienteId", rol, nombre, correo, telefono)
             SELECT gen_random_uuid()::text, c.id, 'PRINCIPAL', c.contacto, c.email,
                    (SELECT l.phone FROM "Lead" l WHERE l."clienteId" = c.id AND l.phone IS NOT NULL AND l.phone <> '' ORDER BY l."updatedAt" DESC LIMIT 1)
               FROM "Cliente" c JOIN "ProyectoFicha" pf ON pf."clienteId" = c.id WHERE pf."solucionId" = $1
             ON CONFLICT ("clienteId", rol) DO NOTHING"#,
        &[B::T(sol.into())],
    )
    .await;
}

// ── Lectura ──────────────────────────────────────────────────────────────────────────────────
async fn cargar_crudo(st: &AppState, sol: &str) -> Result<Option<Value>, sqlx::Error> {
    fetch_json_opt(
        &st.pool,
        r#"SELECT jsonb_build_object(
             'resumen', s.descripcion, 'valor', s."valorEstimado",
             'oportunidad', jsonb_build_object('fuente', pf.fuente, 'presupuesto', pf.presupuesto, 'responsableId', pf."responsableId", 'responsableNombre', pf."responsableNombre"),
             'cliente', (SELECT to_jsonb(c) FROM "Cliente" c WHERE c.id = pf."clienteId"),
             'contactos', COALESCE((SELECT jsonb_agg(to_jsonb(k)) FROM "ClienteContacto" k WHERE k."clienteId" = pf."clienteId"), '[]'::jsonb))
           FROM "Solucion" s LEFT JOIN "ProyectoFicha" pf ON pf."solucionId" = s.id WHERE s.id = $1"#,
        &[B::T(sol.into())],
    )
    .await
}

fn contacto(crudo: &Value, rol: &str) -> Value {
    crudo["contactos"]
        .as_array()
        .and_then(|a| a.iter().find(|k| k["rol"] == rol))
        .map(|k| json!({ "nombre": k["nombre"], "cargo": k["cargo"], "correo": k["correo"], "telefono": k["telefono"] }))
        .unwrap_or(Value::Null)
}

/// La ficha con la forma que usa la pantalla.
pub fn armar(crudo: &Value) -> Value {
    let c = &crudo["cliente"];
    let existe = c.is_object();
    let o = &crudo["oportunidad"];
    json!({
        "existe": existe,
        "cliente": if existe { json!({
            "id": c["id"], "nombre": c["nombre"], "razonSocial": c["razonSocial"], "nit": c["nit"], "sector": c["industria"], "tamano": c["tamano"],
            "ciudad": c["ciudad"], "pais": c["pais"], "sitioWeb": c["sitioWeb"],
        }) } else { Value::Null },
        "contactos": { "principal": contacto(crudo, "PRINCIPAL"), "decisor": contacto(crudo, "DECISOR"), "representante": contacto(crudo, "REPRESENTANTE_LEGAL"), "firmante": contacto(crudo, "FIRMANTE") },
        "oportunidad": { "resumen": crudo["resumen"], "fuente": o["fuente"], "valor": crudo["valor"], "presupuesto": o["presupuesto"], "responsableId": o["responsableId"], "responsableNombre": o["responsableNombre"] },
    })
}

fn falta(campo: &str, etiqueta: &str, motivo: &str) -> Value {
    json!({ "campo": campo, "etiqueta": etiqueta, "motivo": motivo })
}

/// Los 11 datos que pide la fase 1 y los papeles de contacto ya identificados.
pub fn completitud(f: &Value) -> Value {
    let (c, k, o) = (&f["cliente"], &f["contactos"], &f["oportunidad"]);
    let mut faltan: Vec<Value> = Vec::new();
    let total = 11;
    let mut revisa = |ok: bool, campo: &str, etiqueta: &str, motivo: &str| {
        if !ok {
            faltan.push(falta(campo, etiqueta, motivo));
        }
    };
    revisa(!es_relleno(txt(&c["nombre"])), "nombre", "Nombre comercial", "Falta");
    revisa(!es_relleno(txt(&c["razonSocial"])), "razonSocial", "Razón social", "Falta");
    revisa(nit_normalizado(txt(&c["nit"])).is_some(), "nit", "NIT", if txt(&c["nit"]).is_empty() { "Falta" } else { "No parece un NIT (8 a 11 dígitos)" });
    revisa(!es_relleno(txt(&c["sector"])), "sector", "Sector", "Falta");
    revisa(!es_relleno(txt(&c["ciudad"])) && !es_relleno(txt(&c["pais"])), "ciudad", "Ciudad y país", "Falta la ciudad o el país");
    let p = &k["principal"];
    let mut falta_p: Vec<&str> = Vec::new();
    if es_relleno(txt(&p["nombre"])) { falta_p.push("nombre"); }
    if es_relleno(txt(&p["cargo"])) { falta_p.push("cargo"); }
    if !correo_valido(txt(&p["correo"])) { falta_p.push("correo válido"); }
    if !telefono_valido(txt(&p["telefono"])) { falta_p.push("teléfono"); }
    revisa(falta_p.is_empty(), "principal", "Contacto principal", &format!("Falta: {}", falta_p.join(", ")));
    // «Quién decide»: sin persona aparte se entiende que decide el contacto principal
    revisa(k["decisor"].is_null() || !es_relleno(txt(&k["decisor"]["nombre"])), "decisor", "Quién decide la compra", "Falta el nombre");
    revisa(txt(&o["resumen"]).chars().count() >= 10 && !es_relleno(txt(&o["resumen"])), "resumen", "Qué quiere resolver", "Describe en una o dos líneas");
    revisa(!es_relleno(txt(&o["fuente"])), "fuente", "Fuente", "Falta");
    revisa(o["valor"].as_f64().unwrap_or(0.0) > 0.0, "valor", "Valor estimado", "Debe ser mayor que cero");
    revisa(!es_relleno(txt(&o["responsableNombre"])) || !txt(&o["responsableId"]).is_empty(), "responsable", "Responsable", "Falta");
    let mut roles: Vec<&str> = Vec::new();
    for (clave, rol) in ROLES {
        let nombre_ok = !es_relleno(txt(&k[clave]["nombre"]));
        if nombre_ok || (clave == "decisor" && k["decisor"].is_null() && !es_relleno(txt(&p["nombre"]))) {
            roles.push(rol);
        }
    }
    json!({ "ok": total - faltan.len(), "total": total, "faltan": faltan, "roles": roles })
}

/// Para el motor de fases: cuántos datos tiene la ficha del proyecto y qué papeles están identificados.
pub async fn completitud_de(st: &AppState, sol: &str) -> Value {
    sembrar_desde_lead(st, sol).await;
    match cargar_crudo(st, sol).await {
        Ok(Some(crudo)) => completitud(&armar(&crudo)),
        _ => json!({ "ok": 0, "total": 11, "faltan": [], "roles": [] }),
    }
}

async fn obtener(State(st): State<AppState>, _se: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    sembrar_desde_lead(&st, &id).await;
    let crudo = cargar_crudo(&st, &id).await?.ok_or_else(|| ApiError::not_found("Proyecto no encontrado"))?;
    let mut f = armar(&crudo);
    f["completitud"] = completitud(&f);
    Ok(Json(f))
}

async fn buscar(State(st): State<AppState>, _se: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let t = q.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
    if t.chars().count() < 2 {
        return Ok(Json(json!([])));
    }
    let digitos: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    let r = fetch_json(
        &st.pool,
        r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('id', x.id, 'nombre', x.nombre, 'razonSocial', x."razonSocial", 'nit', x.nit)), '[]'::jsonb)
             FROM (SELECT id, nombre, "razonSocial", nit FROM "Cliente"
                    WHERE nombre ILIKE '%' || $1 || '%' OR "razonSocial" ILIKE '%' || $1 || '%' OR ($2 <> '' AND nit LIKE $2 || '%')
                    ORDER BY nombre LIMIT 8) x"#,
        &[B::T(t), B::T(digitos)],
    )
    .await?;
    Ok(Json(r))
}

// ── Escritura ────────────────────────────────────────────────────────────────────────────────
/// Cambia solo las columnas que llegan en el cuerpo (`null` o texto vacío la dejan en NULL). `tabla` se identifica por `col_id`.
async fn actualizar(st: &AppState, tabla: &str, col_id: &str, id: &str, cambios: Vec<(&str, B)>, con_updated: bool) -> Result<(), sqlx::Error> {
    if cambios.is_empty() && !con_updated {
        return Ok(());
    }
    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<B> = vec![B::T(id.into())];
    for (col, b) in cambios {
        binds.push(b);
        sets.push(format!(r#""{col}" = ${}"#, binds.len()));
    }
    if con_updated {
        sets.push(r#""updatedAt" = NOW()"#.into());
    }
    exec(&st.pool, &format!(r#"UPDATE "{tabla}" SET {} WHERE "{col_id}" = $1"#, sets.join(", ")), &binds).await?;
    Ok(())
}

async fn guardar(State(st): State<AppState>, se: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if fetch_text_opt(&st.pool, r#"SELECT id FROM "Solucion" WHERE id = $1"#, &[B::T(id.clone())]).await?.is_none() {
        return Err(ApiError::not_found("Proyecto no encontrado"));
    }
    let quien = if se.name.is_empty() { se.email.clone() } else { se.name.clone() };
    sembrar_desde_lead(&st, &id).await;
    exec(&st.pool, r#"INSERT INTO "ProyectoFicha" ("solucionId") VALUES ($1) ON CONFLICT ("solucionId") DO NOTHING"#, &[B::T(id.clone())]).await?;
    let mut cliente_id = fetch_text_opt(&st.pool, r#"SELECT "clienteId" FROM "ProyectoFicha" WHERE "solucionId" = $1"#, &[B::T(id.clone())]).await?;

    // 1. Empresa: elegir una existente, crear una nueva (nueva: true) o seguir con la que ya tiene
    if body["nueva"] == true {
        cliente_id = None;
    }
    if let Some(elegido) = body["clienteId"].as_str().filter(|c| !c.is_empty()) {
        if fetch_text_opt(&st.pool, r#"SELECT id FROM "Cliente" WHERE id = $1"#, &[B::T(elegido.into())]).await?.is_none() {
            return Err(ApiError::bad_request("Esa empresa no existe"));
        }
        cliente_id = Some(elegido.to_string());
    }
    let datos = &body["cliente"];
    if cliente_id.is_none() {
        if !datos.is_object() || txt(&datos["nombre"]).is_empty() {
            return Err(ApiError::bad_request("Indica el nombre comercial de la empresa o elige una existente"));
        }
        let nuevo = new_id();
        exec(
            &st.pool,
            r#"INSERT INTO "Cliente" (id, nombre, industria, contacto, email, pais, estado, "valorTotal", "updatedAt") VALUES ($1, $2, 'Sin especificar', '', '', 'Sin especificar', 'Activo', 0, NOW())"#,
            &[B::T(nuevo.clone()), B::T(txt(&datos["nombre"]).to_string())],
        )
        .await?;
        cliente_id = Some(nuevo);
    }
    let cid = cliente_id.clone().unwrap_or_default();
    exec(&st.pool, r#"UPDATE "ProyectoFicha" SET "clienteId" = $2, "actualizadoPor" = $3, "updatedAt" = NOW() WHERE "solucionId" = $1"#, &[B::T(id.clone()), B::T(cid.clone()), B::T(quien.clone())]).await?;

    // 2. Datos de la empresa (solo los que llegan)
    if datos.is_object() && body["clienteId"].is_null() {
        let mut cambios: Vec<(&str, B)> = Vec::new();
        if datos.get("nombre").is_some() {
            if txt(&datos["nombre"]).is_empty() {
                return Err(ApiError::bad_request("El nombre comercial no puede quedar vacío"));
            }
            cambios.push(("nombre", B::T(txt(&datos["nombre"]).into())));
        }
        if datos.get("razonSocial").is_some() { cambios.push(("razonSocial", B::OT(opt(&datos["razonSocial"])))); }
        if datos.get("sector").is_some() { cambios.push(("industria", B::T(opt(&datos["sector"]).unwrap_or_else(|| "Sin especificar".into())))); }
        if datos.get("tamano").is_some() { cambios.push(("tamano", B::OT(opt(&datos["tamano"])))); }
        if datos.get("ciudad").is_some() { cambios.push(("ciudad", B::OT(opt(&datos["ciudad"])))); }
        if datos.get("pais").is_some() { cambios.push(("pais", B::T(opt(&datos["pais"]).unwrap_or_else(|| "Sin especificar".into())))); }
        if datos.get("sitioWeb").is_some() { cambios.push(("sitioWeb", B::OT(opt(&datos["sitioWeb"])))); }
        if datos.get("nit").is_some() {
            let nit = match opt(&datos["nit"]) {
                None => None,
                Some(t) => Some(nit_normalizado(&t).ok_or_else(|| ApiError::bad_request("El NIT debe tener entre 8 y 11 dígitos"))?),
            };
            if let Some(n) = &nit {
                if let Some(otro) = fetch_json_opt(&st.pool, r#"SELECT jsonb_build_object('id', id, 'nombre', nombre) FROM "Cliente" WHERE nit = $1 AND id <> $2"#, &[B::T(n.clone()), B::T(cid.clone())]).await? {
                    return Err(ApiError::new(StatusCode::CONFLICT, "Ya existe una empresa con ese NIT").con_extra(json!({ "existente": otro })));
                }
            }
            cambios.push(("nit", B::OT(nit)));
        }
        actualizar(&st, "Cliente", "id", &cid, cambios, true).await?;
    }

    // 3. Personas por papel
    let personas = &body["contactos"];
    if personas.is_object() {
        for (clave, rol) in ROLES {
            let Some(p) = personas.get(clave) else { continue };
            if p.is_null() {
                exec(&st.pool, r#"DELETE FROM "ClienteContacto" WHERE "clienteId" = $1 AND rol = $2"#, &[B::T(cid.clone()), B::T(rol.into())]).await?;
                continue;
            }
            exec(
                &st.pool,
                r#"INSERT INTO "ClienteContacto" (id, "clienteId", rol, nombre, cargo, correo, telefono) VALUES ($1, $2, $3, $4, $5, $6, $7)
                   ON CONFLICT ("clienteId", rol) DO UPDATE SET nombre = EXCLUDED.nombre, cargo = EXCLUDED.cargo, correo = EXCLUDED.correo, telefono = EXCLUDED.telefono, "updatedAt" = NOW()"#,
                &[B::T(new_id()), B::T(cid.clone()), B::T(rol.into()), B::T(txt(&p["nombre"]).into()), B::OT(opt(&p["cargo"])), B::OT(opt(&p["correo"])), B::OT(opt(&p["telefono"]))],
            )
            .await?;
            if clave == "principal" {
                // los demás módulos leen el contacto de Cliente: se mantiene al día
                exec(&st.pool, r#"UPDATE "Cliente" SET contacto = $2, email = $3, telefono = $4, "updatedAt" = NOW() WHERE id = $1"#, &[B::T(cid.clone()), B::T(txt(&p["nombre"]).into()), B::T(txt(&p["correo"]).into()), B::OT(opt(&p["telefono"]))]).await?;
            }
        }
    }

    // 4. La oportunidad
    let op = &body["oportunidad"];
    if op.is_object() {
        let mut cambios: Vec<(&str, B)> = Vec::new();
        if op.get("fuente").is_some() { cambios.push(("fuente", B::OT(opt(&op["fuente"])))); }
        if op.get("presupuesto").is_some() { cambios.push(("presupuesto", B::OF(op["presupuesto"].as_f64().filter(|v| *v > 0.0)))); }
        if let Some(rid) = op["responsableId"].as_str().filter(|r| !r.is_empty()) {
            let nombre = fetch_text_opt(&st.pool, r#"SELECT name FROM "User" WHERE id = $1"#, &[B::T(rid.into())]).await?.unwrap_or_default();
            cambios.push(("responsableId", B::T(rid.into())));
            cambios.push(("responsableNombre", B::T(nombre)));
        }
        actualizar(&st, "ProyectoFicha", "solucionId", &id, cambios, true).await?;
        let mut sol: Vec<(&str, B)> = Vec::new();
        if op.get("resumen").is_some() { sol.push(("descripcion", B::OT(opt(&op["resumen"])))); }
        if op.get("valor").is_some() { sol.push(("valorEstimado", B::OF(op["valor"].as_f64().filter(|v| *v >= 0.0)))); }
        actualizar(&st, "Solucion", "id", &id, sol, true).await?;
    }

    obtener(State(st), se, Path(id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ficha_completa() -> Value {
        json!({
            "existe": true,
            "cliente": { "nombre": "La Promotora Seguros", "razonSocial": "La Promotora Seguros SAS", "nit": "900.123.456-7", "sector": "Seguros", "ciudad": "Medellín", "pais": "Colombia" },
            "contactos": { "principal": { "nombre": "Gabriel Bolaños", "cargo": "Gerente", "correo": "gabriel@promotora.co", "telefono": "+57 300 123 4567" }, "decisor": null, "representante": null, "firmante": null },
            "oportunidad": { "resumen": "Plataforma de gestión operativa", "fuente": "Referido", "valor": 3490000.0, "responsableNombre": "Santiago Ortega", "responsableId": "u1" },
        })
    }

    #[test]
    fn la_ficha_completa_cumple_los_11() {
        let c = completitud(&ficha_completa());
        assert_eq!(c["ok"], 11, "faltan: {}", c["faltan"]);
        assert!(c["roles"].as_array().unwrap().iter().any(|r| r == "PRINCIPAL"));
        assert!(c["roles"].as_array().unwrap().iter().any(|r| r == "DECISOR"), "sin decisor aparte decide el principal");
    }

    #[test]
    fn los_valores_de_relleno_no_cuentan() {
        let mut f = ficha_completa();
        f["contactos"]["principal"]["correo"] = json!("unknow@unknow.com");
        f["cliente"]["sector"] = json!("Sin especificar");
        f["cliente"]["nit"] = json!("123");
        let c = completitud(&f);
        let campos: Vec<&str> = c["faltan"].as_array().unwrap().iter().filter_map(|x| x["campo"].as_str()).collect();
        assert!(campos.contains(&"principal") && campos.contains(&"sector") && campos.contains(&"nit"), "{campos:?}");
        assert_eq!(c["ok"], 8);
    }

    #[test]
    fn una_ficha_vacia_no_cumple_nada() {
        let c = completitud(&armar(&json!({ "contactos": [], "oportunidad": {} })));
        assert_eq!(c["ok"], 1, "solo «quién decide» se da por bueno sin persona aparte: {}", c["faltan"]);
    }

    #[test]
    fn nit_correo_y_telefono() {
        assert_eq!(nit_normalizado("900.123.456-7").as_deref(), Some("9001234567"));
        assert!(nit_normalizado("12345").is_none());
        assert!(correo_valido("ana@empresa.com.co"));
        for malo in ["unknow@unknow.com", "ana@", "ana empresa@x.com", "test@empresa.com", "ana@example.com", ""] {
            assert!(!correo_valido(malo), "{malo}");
        }
        assert!(telefono_valido("+57 300 123 4567"));
        assert!(!telefono_valido("1111111") && !telefono_valido("123") && !telefono_valido(""));
    }

    #[test]
    fn el_decisor_aparte_debe_tener_nombre_y_cuenta_como_papel() {
        let mut f = ficha_completa();
        f["contactos"]["decisor"] = json!({ "nombre": "", "cargo": null });
        assert!(completitud(&f)["faltan"].as_array().unwrap().iter().any(|x| x["campo"] == "decisor"));
        f["contactos"]["decisor"] = json!({ "nombre": "María Ruiz" });
        f["contactos"]["firmante"] = json!({ "nombre": "Carlos Pérez" });
        let c = completitud(&f);
        assert_eq!(c["ok"], 11);
        let roles = c["roles"].as_array().unwrap();
        assert!(roles.iter().any(|r| r == "FIRMANTE") && !roles.iter().any(|r| r == "REPRESENTANTE_LEGAL"));
    }
}
