//! Contabilidad, RR. HH., finanzas, inventario, proveedores y órdenes de compra — MASD PHUB-0001-0010
//! (bloque 2). Paridad con `src/app/api/{contabilidad,rrhh,finanzas,inventario,proveedores,ordenes}/**`.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    error::{ApiError, ApiResult},
    session::Session,
    state::AppState,
    util::{
        exec, fecha_cuerpo, fetch_i64, fetch_json, fetch_json_opt, fetch_text_opt, log_activity, new_id, parse_float, presente, s, s_o_nulo, truthy,
        ts_js_opt, uid, Upd, B,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        // Contabilidad
        .route("/api/contabilidad/asientos", get(asientos_listar).post(asiento_crear))
        .route("/api/contabilidad/asientos/{id}", get(asiento_obtener).delete(asiento_eliminar))
        .route("/api/contabilidad/balance", get(balance))
        .route("/api/contabilidad/cuentas", get(cuentas_listar).post(cuenta_crear))
        .route("/api/contabilidad/cuentas/{id}", put(cuenta_actualizar).delete(cuenta_eliminar))
        .route("/api/contabilidad/movimientos", get(movimientos_listar).post(movimiento_crear))
        .route("/api/contabilidad/movimientos/{id}", put(movimiento_actualizar).delete(movimiento_eliminar))
        // RR. HH.
        .route("/api/rrhh/empleados", get(empleados_listar).post(empleado_crear))
        .route("/api/rrhh/empleados/{id}", put(empleado_actualizar).delete(empleado_eliminar))
        .route("/api/rrhh/nomina", get(nominas_listar).post(nomina_crear))
        .route("/api/rrhh/vacaciones", get(vacaciones_listar).post(vacacion_crear))
        .route("/api/rrhh/vacaciones/{id}", put(vacacion_actualizar))
        // Finanzas
        .route("/api/finanzas", get(finanzas_listar).post(finanza_crear))
        .route("/api/finanzas/{id}", put(finanza_actualizar).delete(finanza_eliminar))
        // Inventario, proveedores, órdenes
        .route("/api/inventario", get(activos_listar).post(activo_crear))
        .route("/api/inventario/{id}", put(activo_actualizar).delete(activo_eliminar))
        .route("/api/proveedores", get(proveedores_listar).post(proveedor_crear))
        .route("/api/proveedores/{id}", put(proveedor_actualizar).delete(proveedor_eliminar))
        .route("/api/ordenes", get(ordenes_listar).post(orden_crear))
        .route("/api/ordenes/{id}", put(orden_actualizar).delete(orden_eliminar))
}

fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}

/// Borra por id; 500 (como la excepción de Prisma) si no existía.
async fn borrar(st: &AppState, tabla: &str, id: &str) -> ApiResult<Json<Value>> {
    let n = exec(&st.pool, &format!(r#"DELETE FROM "{tabla}" WHERE id = $1"#), &[B::T(id.to_string())]).await?;
    if n == 0 {
        return Err(ApiError::internal("Error interno"));
    }
    Ok(ok())
}

/// Inserta con `INSERT ... RETURNING *` y devuelve la fila como JSON.
async fn actualizar(st: &AppState, up: &Upd, tabla: &str) -> ApiResult<Value> {
    fetch_json_opt(&st.pool, &up.sql(tabla), &up.binds).await?.ok_or_else(|| ApiError::internal("Error interno"))
}

const CUENTA_CONTABLE: &str = r#"to_jsonb(cu)"#;
const LINEA_CON_CUENTA: &str = r#"to_jsonb(l) || jsonb_build_object('cuenta', (SELECT to_jsonb(cu) FROM "CuentaContable" cu WHERE cu.id = l."cuentaId"))"#;
const ASIENTO_JSON: &str = r#"to_jsonb(a) || jsonb_build_object('lineas', COALESCE((SELECT jsonb_agg(to_jsonb(l) || jsonb_build_object('cuenta', (SELECT to_jsonb(cu) FROM "CuentaContable" cu WHERE cu.id = l."cuentaId")) ORDER BY l."createdAt") FROM "LineaAsiento" l WHERE l."asientoId" = a.id), '[]'::jsonb))"#;

// ═══════════════════════════════ CONTABILIDAD ═══════════════════════════════
async fn asientos_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let sql = format!(r#"SELECT COALESCE(jsonb_agg({ASIENTO_JSON} ORDER BY a.numero DESC), '[]'::jsonb) FROM "AsientoContable" a"#);
    Ok(Json(fetch_json(&st.pool, &sql, &[]).await?))
}

async fn asiento_obtener(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let sql = format!(r#"SELECT {ASIENTO_JSON} FROM "AsientoContable" a WHERE a.id = $1"#);
    fetch_json_opt(&st.pool, &sql, &[B::T(id)]).await?.map(Json).ok_or_else(|| ApiError::not_found("Not found"))
}

async fn asiento_eliminar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    borrar(&st, "AsientoContable", &id).await
}

async fn asiento_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let lineas = body.get("lineas").and_then(|v| v.as_array()).ok_or_else(|| ApiError::internal("Error interno"))?.clone();
    let total_debe: f64 = lineas.iter().map(|l| parse_float(l.get("debe"))).sum();
    let total_haber: f64 = lineas.iter().map(|l| parse_float(l.get("haber"))).sum();
    if (total_debe - total_haber).abs() > 0.01 {
        return Err(ApiError::bad_request("El asiento no cuadra: Debe ≠ Haber"));
    }
    let numero = fetch_i64(&st.pool, r#"SELECT COUNT(*) FROM "AsientoContable""#, &[]).await? + 1;
    let id = new_id();
    let mut tx = st.pool.begin().await?;
    sqlx::query(
        r#"INSERT INTO "AsientoContable" (id, numero, fecha, descripcion, referencia, estado, tipo, "creadoPor", "createdAt", "updatedAt")
           VALUES ($1, $2::int, $3::text::timestamptz AT TIME ZONE 'UTC', $4, $5, COALESCE($6, 'CONFIRMADO'), COALESCE($7, 'MANUAL'), $8, NOW(), NOW())"#,
    )
    .bind(&id)
    .bind(numero)
    .bind(s(&body, "fecha"))
    .bind(s(&body, "descripcion"))
    .bind(s_o_nulo(&body, "referencia"))
    .bind(s_o_nulo(&body, "estado"))
    .bind(s_o_nulo(&body, "tipo"))
    .bind(s_o_nulo(&body, "creadoPor"))
    .execute(&mut *tx)
    .await?;
    for l in &lineas {
        sqlx::query(r#"INSERT INTO "LineaAsiento" (id, "asientoId", "cuentaId", descripcion, debe, haber, "createdAt") VALUES ($1, $2, $3, $4, $5, $6, NOW())"#)
            .bind(new_id())
            .bind(&id)
            .bind(s(l, "cuentaId"))
            .bind(s_o_nulo(l, "descripcion"))
            .bind(parse_float(l.get("debe")))
            .bind(parse_float(l.get("haber")))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    let sql = format!(r#"SELECT {ASIENTO_JSON} FROM "AsientoContable" a WHERE a.id = $1"#);
    let v = fetch_json(&st.pool, &sql, &[B::T(id)]).await?;
    Ok((StatusCode::CREATED, Json(v)))
}

async fn balance(State(st): State<AppState>, _s: Session, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let desde = q.get("desde").filter(|x| !x.is_empty()).cloned();
    let hasta = q.get("hasta").filter(|x| !x.is_empty()).cloned();
    // Con filtro de fechas solo cuentan las líneas de asientos dentro del rango (`hasta` incluye todo ese día).
    let sql = r#"SELECT COALESCE(jsonb_agg(jsonb_build_object('c', to_jsonb(cu), 'debe', COALESCE(t.debe, 0), 'haber', COALESCE(t.haber, 0)) ORDER BY cu.codigo ASC), '[]'::jsonb)
        FROM "CuentaContable" cu
        LEFT JOIN LATERAL (SELECT SUM(l.debe) debe, SUM(l.haber) haber FROM "LineaAsiento" l JOIN "AsientoContable" a ON a.id = l."asientoId"
             WHERE l."cuentaId" = cu.id
               AND ($1::text IS NULL OR a.fecha >= ($1::text::timestamptz AT TIME ZONE 'UTC'))
               AND ($2::text IS NULL OR a.fecha <= ((($2::text || 'T23:59:59')::timestamptz) AT TIME ZONE 'UTC'))) t ON true
        WHERE cu.activa = true"#;
    let filas = fetch_json(&st.pool, sql, &[B::OT(desde), B::OT(hasta)]).await?;
    let mut cuentas = vec![];
    let mut tot = [0.0f64; 5];
    for fila in filas.as_array().cloned().unwrap_or_default() {
        let mut c = fila["c"].clone();
        let debe = fila["debe"].as_f64().unwrap_or(0.0);
        let haber = fila["haber"].as_f64().unwrap_or(0.0);
        let tipo = c["tipo"].as_str().unwrap_or("").to_string();
        let saldo = if tipo == "ACTIVO" || tipo == "GASTO" { debe - haber } else { haber - debe };
        c["totalDebe"] = json!(debe);
        c["totalHaber"] = json!(haber);
        c["saldo"] = json!(saldo);
        match tipo.as_str() {
            "ACTIVO" => tot[0] += saldo,
            "PASIVO" => tot[1] += saldo,
            "PATRIMONIO" => tot[2] += saldo,
            "INGRESO" => tot[3] += saldo,
            "GASTO" => tot[4] += saldo,
            _ => {}
        }
        cuentas.push(c);
    }
    Ok(Json(json!({
        "cuentas": cuentas,
        "totals": { "ACTIVO": tot[0], "PASIVO": tot[1], "PATRIMONIO": tot[2], "INGRESO": tot[3], "GASTO": tot[4] },
        "utilidad": tot[3] - tot[4],
    })))
}

async fn cuentas_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    Ok(Json(fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c.codigo ASC), '[]'::jsonb) FROM "CuentaContable" c"#, &[]).await?))
}

async fn cuenta_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let v = fetch_json(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "CuentaContable" (id, codigo, nombre, tipo, subtipo, nivel, "cuentaPadreId", activa, descripcion, "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, $6::float8::int, $7, $8, $9, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(new_id()),
            B::OT(s(&body, "codigo")),
            B::OT(s(&body, "nombre")),
            B::OT(s(&body, "tipo")),
            B::OT(s_o_nulo(&body, "subtipo")),
            B::F(nivel(&body)),
            B::OT(s_o_nulo(&body, "cuentaPadreId")),
            B::Bo(body.get("activa") != Some(&Value::Bool(false))),
            B::OT(s_o_nulo(&body, "descripcion")),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(v)))
}

/// `parseInt(x) || 1`
fn nivel(body: &Value) -> f64 {
    let n = parse_float(body.get("nivel")).trunc();
    if n == 0.0 {
        1.0
    } else {
        n
    }
}

async fn cuenta_actualizar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut up = Upd::new(&id);
    for k in ["codigo", "nombre", "tipo"] {
        if let Some(v) = s(&body, k) {
            up.set(k, B::T(v));
        }
    }
    up.set("subtipo", B::OT(s_o_nulo(&body, "subtipo")));
    up.set_expr("nivel", B::F(nivel(&body)), "{n}::float8::int");
    up.set("cuentaPadreId", B::OT(s_o_nulo(&body, "cuentaPadreId")));
    up.set("activa", B::Bo(body.get("activa") != Some(&Value::Bool(false))));
    up.set("descripcion", B::OT(s_o_nulo(&body, "descripcion")));
    Ok(Json(actualizar(&st, &up, "CuentaContable").await?))
}

async fn cuenta_eliminar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    borrar(&st, "CuentaContable", &id).await
}

async fn movimientos_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    Ok(Json(fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(m) ORDER BY m.fecha DESC), '[]'::jsonb) FROM "MovimientoBancario" m"#, &[]).await?))
}

async fn movimiento_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let saldo = if truthy(&body, "saldo") { Some(parse_float(body.get("saldo"))) } else { None };
    let sql = format!(
        r#"WITH ins AS (INSERT INTO "MovimientoBancario" (id, fecha, descripcion, referencia, monto, tipo, saldo, conciliado, "asientoId", banco, "createdAt", "updatedAt")
             VALUES ($1, {}, $3, $4, $5, $6, $7, $8, $9, COALESCE($10, 'Principal'), NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        ts_js_opt(2)
    );
    let v = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::OT(s(&body, "fecha")),
            B::OT(s(&body, "descripcion")),
            B::OT(s_o_nulo(&body, "referencia")),
            B::F(parse_float(body.get("monto"))),
            B::OT(s(&body, "tipo")),
            B::OF(saldo),
            B::Bo(truthy(&body, "conciliado")),
            B::OT(s_o_nulo(&body, "asientoId")),
            B::OT(s_o_nulo(&body, "banco")),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(v)))
}

async fn movimiento_actualizar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut up = Upd::new(&id);
    if let Some(c) = body.get("conciliado").and_then(|v| v.as_bool()) {
        up.set("conciliado", B::Bo(c));
    }
    up.set("asientoId", B::OT(s_o_nulo(&body, "asientoId")));
    if let Some(d) = s(&body, "descripcion") {
        up.set("descripcion", B::T(d));
    }
    up.set("referencia", B::OT(s_o_nulo(&body, "referencia")));
    Ok(Json(actualizar(&st, &up, "MovimientoBancario").await?))
}

async fn movimiento_eliminar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    borrar(&st, "MovimientoBancario", &id).await
}

// ═══════════════════════════════ RR. HH. ═══════════════════════════════
const EMPLEADO_JSON: &str = r#"to_jsonb(e) || jsonb_build_object(
    'nominas', COALESCE((SELECT jsonb_agg(to_jsonb(n)) FROM "RegistroNomina" n WHERE n."empleadoId" = e.id), '[]'::jsonb),
    'vacaciones', COALESCE((SELECT jsonb_agg(to_jsonb(v)) FROM "SolicitudVacacion" v WHERE v."empleadoId" = e.id), '[]'::jsonb))"#;

async fn empleados_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let sql = format!(r#"SELECT COALESCE(jsonb_agg({EMPLEADO_JSON} ORDER BY e.nombre ASC), '[]'::jsonb) FROM "EmpleadoRRHH" e"#);
    Ok(Json(fetch_json(&st.pool, &sql, &[]).await?))
}

async fn empleado_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let sql = format!(
        r#"WITH ins AS (INSERT INTO "EmpleadoRRHH" (id, nombre, email, cargo, departamento, tipo, estado, "salarioBase", moneda, "fechaIngreso", pais, "userId", notas, "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, COALESCE($6, 'FULL_TIME'), COALESCE($7, 'ACTIVO'), $8, COALESCE($9, 'USD'), {}, $11, $12, $13, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        ts_js_opt(10)
    );
    let v = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::OT(s(&body, "nombre")),
            B::OT(s(&body, "email")),
            B::OT(s(&body, "cargo")),
            B::OT(s(&body, "departamento")),
            B::OT(s_o_nulo(&body, "tipo")),
            B::OT(s_o_nulo(&body, "estado")),
            B::F(parse_float(body.get("salarioBase"))),
            B::OT(s_o_nulo(&body, "moneda")),
            B::OT(s(&body, "fechaIngreso")),
            B::OT(s_o_nulo(&body, "pais")),
            B::OT(s_o_nulo(&body, "userId")),
            B::OT(s_o_nulo(&body, "notas")),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(v)))
}

async fn empleado_actualizar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut up = Upd::new(&id);
    for k in ["nombre", "email", "cargo", "departamento", "tipo", "estado"] {
        if let Some(v) = s(&body, k) {
            up.set(k, B::T(v));
        }
    }
    up.set("salarioBase", B::F(parse_float(body.get("salarioBase"))));
    up.set("moneda", B::T(s_o_nulo(&body, "moneda").unwrap_or_else(|| "USD".into())));
    up.set_expr("fechaIngreso", B::OT(s(&body, "fechaIngreso")), &ts_js_opt(0).replace("$0", "{n}"));
    up.set_expr("fechaBaja", B::OT(fecha_cuerpo(&body, "fechaBaja")), &ts_js_opt(0).replace("$0", "{n}"));
    up.set("pais", B::OT(s_o_nulo(&body, "pais")));
    up.set("notas", B::OT(s_o_nulo(&body, "notas")));
    Ok(Json(actualizar(&st, &up, "EmpleadoRRHH").await?))
}

async fn empleado_eliminar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    borrar(&st, "EmpleadoRRHH", &id).await
}

const NOMINA_JSON: &str = r#"to_jsonb(n) || jsonb_build_object('empleado', (SELECT to_jsonb(e) FROM "EmpleadoRRHH" e WHERE e.id = n."empleadoId"))"#;
const VACACION_JSON: &str = r#"to_jsonb(v) || jsonb_build_object('empleado', (SELECT to_jsonb(e) FROM "EmpleadoRRHH" e WHERE e.id = v."empleadoId"))"#;

async fn nominas_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let sql = format!(r#"SELECT COALESCE(jsonb_agg({NOMINA_JSON} ORDER BY n."createdAt" DESC), '[]'::jsonb) FROM "RegistroNomina" n"#);
    Ok(Json(fetch_json(&st.pool, &sql, &[]).await?))
}

async fn nomina_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let base = parse_float(body.get("salarioBase"));
    let bonos = parse_float(body.get("bonos"));
    let deducciones = parse_float(body.get("deducciones"));
    let sql = format!(
        r#"WITH n AS (INSERT INTO "RegistroNomina" (id, "empleadoId", periodo, "salarioBase", bonos, deducciones, total, moneda, estado, "fechaPago", notas, "createdAt")
             VALUES ($1, $2, $3, $4, $5, $6, $7, COALESCE($8, 'USD'), COALESCE($9, 'PENDIENTE'), {}, $11, NOW()) RETURNING *) SELECT {NOMINA_JSON} FROM n"#,
        ts_js_opt(10)
    );
    let v = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::OT(s(&body, "empleadoId")),
            B::OT(s(&body, "periodo")),
            B::F(base),
            B::F(bonos),
            B::F(deducciones),
            B::F(base + bonos - deducciones),
            B::OT(s_o_nulo(&body, "moneda")),
            B::OT(s_o_nulo(&body, "estado")),
            B::OT(fecha_cuerpo(&body, "fechaPago")),
            B::OT(s_o_nulo(&body, "notas")),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(v)))
}

async fn vacaciones_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let sql = format!(r#"SELECT COALESCE(jsonb_agg({VACACION_JSON} ORDER BY v."createdAt" DESC), '[]'::jsonb) FROM "SolicitudVacacion" v"#);
    Ok(Json(fetch_json(&st.pool, &sql, &[]).await?))
}

async fn vacacion_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    // `dias: body.dias || diff` con `diff = ceil((hasta - desde) / día) + 1`.
    let sql = format!(
        r#"WITH v AS (INSERT INTO "SolicitudVacacion" (id, "empleadoId", desde, hasta, dias, tipo, estado, "aprobadoPor", notas, "createdAt")
             VALUES ($1, $2, {d}, {h}, COALESCE($5::float8::int, (CEIL(EXTRACT(EPOCH FROM (({h}) - ({d}))) / 86400.0) + 1)::int),
                     COALESCE($6, 'VACACION'), COALESCE($7, 'PENDIENTE'), $8, $9, NOW()) RETURNING *) SELECT {VACACION_JSON} FROM v"#,
        d = ts_js_opt(3),
        h = ts_js_opt(4),
    );
    let dias = if truthy(&body, "dias") { Some(parse_float(body.get("dias"))) } else { None };
    let v = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::OT(s(&body, "empleadoId")),
            B::OT(s(&body, "desde")),
            B::OT(s(&body, "hasta")),
            B::OF(dias),
            B::OT(s_o_nulo(&body, "tipo")),
            B::OT(s_o_nulo(&body, "estado")),
            B::OT(s_o_nulo(&body, "aprobadoPor")),
            B::OT(s_o_nulo(&body, "notas")),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(v)))
}

async fn vacacion_actualizar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut up = Upd::sin_updated(&id);
    if let Some(e) = s(&body, "estado") {
        up.set("estado", B::T(e));
    }
    up.set("aprobadoPor", B::OT(s_o_nulo(&body, "aprobadoPor")));
    up.set("notas", B::OT(s_o_nulo(&body, "notas")));
    let sql = up.con("SolicitudVacacion", &format!("SELECT {VACACION_JSON} FROM up v"));
    fetch_json_opt(&st.pool, &sql, &up.binds).await?.map(Json).ok_or_else(|| ApiError::internal("Error interno"))
}

// ═══════════════════════════════ FINANZAS ═══════════════════════════════
async fn finanzas_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    Ok(Json(fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(r) ORDER BY r.fecha DESC), '[]'::jsonb) FROM "RegistroFinanciero" r"#, &[]).await?))
}

async fn finanza_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"WITH ins AS (INSERT INTO "RegistroFinanciero" (id, fecha, tipo, categoria, concepto, monto, moneda, proyecto, estado, responsable, "createdAt", "updatedAt")
                 VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, 'USD'), $8, COALESCE($9, 'pagado'), $10, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
            &[
                B::T(new_id()),
                B::OT(s(&body, "fecha")),
                B::OT(s(&body, "tipo")),
                B::OT(s(&body, "categoria")),
                B::OT(s(&body, "concepto")),
                B::F(parse_float(body.get("monto"))),
                B::OT(s(&body, "moneda")),
                B::OT(s(&body, "proyecto")),
                B::OT(s(&body, "estado")),
                B::OT(s(&body, "responsable")),
            ],
        )
        .await?,
    ))
}

async fn finanza_actualizar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let fallo = || ApiError::internal("Error al actualizar el registro");
    let mut up = Upd::new(&id);
    for k in ["fecha", "tipo", "categoria", "concepto", "moneda", "proyecto", "estado", "responsable"] {
        if let Some(v) = s(&body, k) {
            up.set(k, B::T(v));
        }
    }
    up.set("monto", B::F(parse_float(body.get("monto"))));
    match fetch_json_opt(&st.pool, &up.sql("RegistroFinanciero"), &up.binds).await {
        Ok(Some(v)) => Ok(Json(v)),
        _ => Err(fallo()),
    }
}

async fn finanza_eliminar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    match exec(&st.pool, r#"DELETE FROM "RegistroFinanciero" WHERE id = $1"#, &[B::T(id)]).await {
        Ok(n) if n > 0 => Ok(ok()),
        _ => Err(ApiError::internal("Error al eliminar el registro")),
    }
}

// ═══════════════════════════════ INVENTARIO ═══════════════════════════════
async fn activos_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    Ok(Json(fetch_json(&st.pool, r#"SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a."createdAt" DESC), '[]'::jsonb) FROM "Activo" a"#, &[]).await?))
}

async fn activo_crear(State(st): State<AppState>, sesion: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let sql = format!(
        r#"WITH ins AS (INSERT INTO "Activo" (id, nombre, tipo, categoria, estado, valor, moneda, "fechaAdquisicion", "fechaVencimiento", "proveedorNombre", responsable, ubicacion, serial, notas, "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, COALESCE($5, 'ACTIVO'), $6, COALESCE($7, 'USD'), {}, {}, $10, $11, $12, $13, $14, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        ts_js_opt(8),
        ts_js_opt(9)
    );
    let v = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::OT(s(&body, "nombre")),
            B::OT(s(&body, "tipo")),
            B::OT(s_o_nulo(&body, "categoria")),
            B::OT(s_o_nulo(&body, "estado")),
            B::F(parse_float(body.get("valor"))),
            B::OT(s_o_nulo(&body, "moneda")),
            B::OT(fecha_cuerpo(&body, "fechaAdquisicion")),
            B::OT(fecha_cuerpo(&body, "fechaVencimiento")),
            B::OT(s_o_nulo(&body, "proveedorNombre")),
            B::OT(s_o_nulo(&body, "responsable")),
            B::OT(s_o_nulo(&body, "ubicacion")),
            B::OT(s_o_nulo(&body, "serial")),
            B::OT(s_o_nulo(&body, "notas")),
        ],
    )
    .await?;
    let id = v["id"].as_str().unwrap_or_default().to_string();
    log_activity(&st.pool, "CREATED", &format!("registró el activo {}", s(&body, "nombre").unwrap_or_default()), "activo", &id, uid(&sesion), None).await;
    Ok((StatusCode::CREATED, Json(v)))
}

async fn activo_actualizar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut up = Upd::new(&id);
    for k in ["nombre", "tipo", "estado"] {
        if let Some(v) = s(&body, k) {
            up.set(k, B::T(v));
        }
    }
    up.set("categoria", B::OT(s_o_nulo(&body, "categoria")));
    up.set("valor", B::F(parse_float(body.get("valor"))));
    up.set("moneda", B::T(s_o_nulo(&body, "moneda").unwrap_or_else(|| "USD".into())));
    for k in ["fechaAdquisicion", "fechaVencimiento"] {
        up.set_expr(k, B::OT(fecha_cuerpo(&body, k)), &ts_js_opt(0).replace("$0", "{n}"));
    }
    for k in ["proveedorNombre", "responsable", "ubicacion", "serial", "notas"] {
        up.set(k, B::OT(s_o_nulo(&body, k)));
    }
    let v = actualizar(&st, &up, "Activo").await?;
    log_activity(&st.pool, "UPDATED", &format!("actualizó el activo {}", s(&body, "nombre").unwrap_or_default()), "activo", &id, uid(&sesion), None).await;
    Ok(Json(v))
}

async fn activo_eliminar(State(st): State<AppState>, sesion: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let nombre = fetch_text_opt(&st.pool, r#"SELECT nombre FROM "Activo" WHERE id = $1"#, &[B::T(id.clone())]).await?;
    let r = borrar(&st, "Activo", &id).await?;
    log_activity(&st.pool, "UPDATED", &format!("eliminó el activo {}", nombre.unwrap_or_default()), "activo", &id, uid(&sesion), None).await;
    Ok(r)
}

// ═══════════════════════════════ PROVEEDORES Y ÓRDENES ═══════════════════════════════
async fn proveedores_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    Ok(Json(
        fetch_json(
            &st.pool,
            r#"SELECT COALESCE(jsonb_agg(to_jsonb(p) || jsonb_build_object('ordenes', COALESCE((SELECT jsonb_agg(to_jsonb(o)) FROM "OrdenCompra" o WHERE o."proveedorId" = p.id), '[]'::jsonb))
                                        ORDER BY p."createdAt" DESC), '[]'::jsonb) FROM "Proveedor" p"#,
            &[],
        )
        .await?,
    ))
}

async fn proveedor_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let v = fetch_json(
        &st.pool,
        r#"WITH ins AS (INSERT INTO "Proveedor" (id, nombre, tipo, contacto, email, telefono, pais, website, estado, notas, "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, COALESCE($9, 'ACTIVO'), $10, NOW(), NOW()) RETURNING *) SELECT to_jsonb(ins) FROM ins"#,
        &[
            B::T(new_id()),
            B::OT(s(&body, "nombre")),
            B::OT(s(&body, "tipo")),
            B::OT(s_o_nulo(&body, "contacto")),
            B::OT(s_o_nulo(&body, "email")),
            B::OT(s_o_nulo(&body, "telefono")),
            B::OT(s_o_nulo(&body, "pais")),
            B::OT(s_o_nulo(&body, "website")),
            B::OT(s_o_nulo(&body, "estado")),
            B::OT(s_o_nulo(&body, "notas")),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(v)))
}

async fn proveedor_actualizar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut up = Upd::new(&id);
    for k in ["nombre", "tipo", "estado"] {
        if let Some(v) = s(&body, k) {
            up.set(k, B::T(v));
        }
    }
    for k in ["contacto", "email", "telefono", "pais", "website", "notas"] {
        up.set(k, B::OT(s_o_nulo(&body, k)));
    }
    Ok(Json(actualizar(&st, &up, "Proveedor").await?))
}

async fn proveedor_eliminar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    borrar(&st, "Proveedor", &id).await
}

const ORDEN_JSON: &str = r#"to_jsonb(o) || jsonb_build_object('proveedor', (SELECT to_jsonb(p) FROM "Proveedor" p WHERE p.id = o."proveedorId"))"#;

async fn ordenes_listar(State(st): State<AppState>, _s: Session) -> ApiResult<Json<Value>> {
    let sql = format!(r#"SELECT COALESCE(jsonb_agg({ORDEN_JSON} ORDER BY o."createdAt" DESC), '[]'::jsonb) FROM "OrdenCompra" o"#);
    Ok(Json(fetch_json(&st.pool, &sql, &[]).await?))
}

async fn orden_crear(State(st): State<AppState>, _s: Session, Json(body): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let numero = format!("OC-{:04}", fetch_i64(&st.pool, r#"SELECT COUNT(*) FROM "OrdenCompra""#, &[]).await? + 1);
    let sql = format!(
        r#"WITH o AS (INSERT INTO "OrdenCompra" (id, numero, concepto, descripcion, monto, moneda, estado, "proveedorId", "fechaEmision", "fechaVencimiento", "fechaPago", categoria, "aprobadoPor", notas, "createdAt", "updatedAt")
             VALUES ($1, $2, $3, $4, $5, COALESCE($6, 'USD'), COALESCE($7, 'PENDIENTE'), $8, COALESCE({}, NOW()), {}, {}, $12, $13, $14, NOW(), NOW()) RETURNING *) SELECT {ORDEN_JSON} FROM o"#,
        ts_js_opt(9),
        ts_js_opt(10),
        ts_js_opt(11)
    );
    let v = fetch_json(
        &st.pool,
        &sql,
        &[
            B::T(new_id()),
            B::T(numero),
            B::OT(s(&body, "concepto")),
            B::OT(s_o_nulo(&body, "descripcion")),
            B::F(parse_float(body.get("monto"))),
            B::OT(s_o_nulo(&body, "moneda")),
            B::OT(s_o_nulo(&body, "estado")),
            B::OT(s(&body, "proveedorId")),
            B::OT(fecha_cuerpo(&body, "fechaEmision")),
            B::OT(fecha_cuerpo(&body, "fechaVencimiento")),
            B::OT(fecha_cuerpo(&body, "fechaPago")),
            B::OT(s_o_nulo(&body, "categoria")),
            B::OT(s_o_nulo(&body, "aprobadoPor")),
            B::OT(s_o_nulo(&body, "notas")),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(v)))
}

async fn orden_actualizar(State(st): State<AppState>, _s: Session, Path(id): Path<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut up = Upd::new(&id);
    for k in ["concepto", "estado", "proveedorId"] {
        if let Some(v) = s(&body, k) {
            up.set(k, B::T(v));
        }
    }
    up.set("descripcion", B::OT(s_o_nulo(&body, "descripcion")));
    up.set("monto", B::F(parse_float(body.get("monto"))));
    up.set("moneda", B::T(s_o_nulo(&body, "moneda").unwrap_or_else(|| "USD".into())));
    for k in ["fechaVencimiento", "fechaPago"] {
        up.set_expr(k, B::OT(fecha_cuerpo(&body, k)), &ts_js_opt(0).replace("$0", "{n}"));
    }
    for k in ["categoria", "aprobadoPor", "notas"] {
        up.set(k, B::OT(s_o_nulo(&body, k)));
    }
    let sql = up.con("OrdenCompra", &format!("SELECT {ORDEN_JSON} FROM up o"));
    fetch_json_opt(&st.pool, &sql, &up.binds).await?.map(Json).ok_or_else(|| ApiError::internal("Error interno"))
}

async fn orden_eliminar(State(st): State<AppState>, _s: Session, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    borrar(&st, "OrdenCompra", &id).await
}

#[allow(dead_code)]
fn _no_usados() {
    let _ = (presente, CUENTA_CONTABLE, LINEA_CON_CUENTA);
}
