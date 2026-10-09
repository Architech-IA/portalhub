#!/usr/bin/env node
/**
 * Paridad de autenticación Next (NextAuth v4, :3003) ↔ Rust (portalhub, :3100).
 *
 * Ejecuta el mismo recorrido contra las dos implementaciones con un usuario de prueba y verifica, además,
 * que las sesiones son INTERCAMBIABLES: la cookie que emite Rust la entiende Next y al revés.
 *
 *   set -a; . /root/portal-architechia/.env; set +a
 *   node /root/repos/portalhub/tools/parity/auth.js
 */
const path = require('path')
const ROOT = '/root/portal-architechia'
const { hash } = require(path.join(ROOT, 'node_modules/bcryptjs'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))

const NEXT = process.env.PARITY_NEXT || 'http://127.0.0.1:3003'
const RS = process.env.PARITY_RS || 'http://127.0.0.1:3100'
const prisma = new PrismaClient()
const CORREO = 'parity-auth@test.local'
const CLAVE = 'Parity-Auth-123'
const res = { ok: 0, falla: 0 }

function chequeo(nombre, cond, detalle) {
  if (cond) { res.ok++; console.log(`  ✓ ${nombre}`) } else { res.falla++; console.log(`  ✗ ${nombre}${detalle ? ' — ' + detalle : ''}`) }
}

// Jarra de cookies mínima: nombre → valor (decodificado)
class Jarra {
  constructor() { this.c = new Map() }
  guardar(r) {
    for (const sc of (r.headers.getSetCookie ? r.headers.getSetCookie() : [])) {
      const [par, ...attrs] = sc.split(';')
      const i = par.indexOf('=')
      const k = par.slice(0, i).trim(), v = par.slice(i + 1).trim()
      const borrar = attrs.some(a => /max-age=0/i.test(a)) || v === ''
      if (borrar) this.c.delete(k); else this.c.set(k, v)
    }
  }
  cab() { return [...this.c.entries()].map(([k, v]) => `${k}=${v}`).join('; ') }
}

async function pedir(base, jarra, metodo, url, cuerpo, extra = {}) {
  const headers = { ...(jarra.c.size ? { cookie: jarra.cab() } : {}), ...extra }
  let body
  if (cuerpo !== undefined) { body = new URLSearchParams(cuerpo).toString(); headers['content-type'] = 'application/x-www-form-urlencoded' }
  const r = await fetch(base + url, { method: metodo, headers, body, redirect: 'manual' })
  jarra.guardar(r)
  const texto = await r.text()
  let json = null
  try { json = JSON.parse(texto) } catch { /* no es JSON */ }
  return { status: r.status, json, texto: texto.slice(0, 200), loc: r.headers.get('location'), sc: r.headers.getSetCookie ? r.headers.getSetCookie() : [] }
}

const nombresCookie = (sc) => sc.map(s => s.split('=')[0]).sort().join(',')
const atributos = (sc, nombre) => { const s = sc.find(x => x.startsWith(nombre + '=')); return s ? s.split(';').slice(1).map(a => a.trim().toLowerCase().replace(/=.*/, '')).sort().join(',') : null }

async function recorrido(base, etiqueta) {
  const j = new Jarra()
  const sal = {}
  sal.proveedores = await pedir(base, j, 'GET', '/api/auth/providers')
  sal.sesionVacia = await pedir(base, j, 'GET', '/api/auth/session')
  sal.csrf = await pedir(base, j, 'GET', '/api/auth/csrf')
  const token = sal.csrf.json && sal.csrf.json.csrfToken
  sal.malCsrf = await pedir(base, j, 'POST', '/api/auth/callback/credentials', { csrfToken: 'x', email: CORREO, password: CLAVE, json: 'true', callbackUrl: '/' })
  sal.malaClave = await pedir(base, j, 'POST', '/api/auth/callback/credentials', { csrfToken: token, email: CORREO, password: 'nope', json: 'true', callbackUrl: '/' })
  sal.sinUsuario = await pedir(base, j, 'POST', '/api/auth/callback/credentials', { csrfToken: token, email: 'no-existe@test.local', password: 'x', json: 'true', callbackUrl: '/' })
  sal.login = await pedir(base, j, 'POST', '/api/auth/callback/credentials', { csrfToken: token, email: CORREO, password: CLAVE, json: 'true', callbackUrl: '/profile' })
  sal.cookieSesion = [...j.c.keys()].find(k => /session-token/.test(k))
  sal.valorSesion = sal.cookieSesion ? j.c.get(sal.cookieSesion) : null
  sal.sesion = await pedir(base, j, 'GET', '/api/auth/session')
  sal.loginHtml = await (async () => {
    const j2 = new Jarra()
    const c = await pedir(base, j2, 'GET', '/api/auth/csrf')
    return pedir(base, j2, 'POST', '/api/auth/callback/credentials', { csrfToken: c.json.csrfToken, email: CORREO, password: CLAVE, callbackUrl: 'https://evil.example.com/x' })
  })()
  sal.signout = await pedir(base, j, 'POST', '/api/auth/signout', { csrfToken: token, json: 'true', callbackUrl: '/login' })
  sal.sesionTrasSalir = await pedir(base, j, 'GET', '/api/auth/session')
  sal.signinPagina = await pedir(base, new Jarra(), 'GET', '/api/auth/signin?callbackUrl=%2Fbacklog')
  sal.errorPagina = await pedir(base, new Jarra(), 'GET', '/api/auth/error?error=CredentialsSignin')
  return sal
}

;(async () => {
  const usuario = await prisma.user.upsert({
    where: { email: CORREO },
    update: { password: await hash(CLAVE, 10) },
    create: { name: 'Parity Auth', email: CORREO, password: await hash(CLAVE, 10), role: 'PARTNER' },
  })
  try {
    console.log('\n── recorrido en Next')
    const n = await recorrido(NEXT, 'Next')
    console.log('── recorrido en Rust')
    const r = await recorrido(RS, 'Rust')

    console.log('\n── comparación')
    const mismo = (a, b) => JSON.stringify(a) === JSON.stringify(b)
    chequeo('providers idénticos', mismo(n.proveedores.json, r.proveedores.json), JSON.stringify(n.proveedores.json) + ' ≠ ' + JSON.stringify(r.proveedores.json))
    chequeo('session sin cookie: {} en ambos', mismo(n.sesionVacia.json, {}) && mismo(r.sesionVacia.json, {}), JSON.stringify(n.sesionVacia.json) + ' / ' + JSON.stringify(r.sesionVacia.json))
    chequeo('csrf: token hex de 64 en ambos', /^[0-9a-f]{64}$/.test(n.csrf.json.csrfToken) && /^[0-9a-f]{64}$/.test(r.csrf.json.csrfToken))
    chequeo('csrf: deja la cookie __Host-next-auth.csrf-token (Rust)', nombresCookie(r.csrf.sc).includes('next-auth.csrf-token'), nombresCookie(r.csrf.sc))
    chequeo('csrf: atributos de la cookie (Rust)', /httponly/.test(atributos(r.csrf.sc, r.csrf.sc[0].split('=')[0]) || ''), '')
    chequeo('csrf inválido: mismo status y forma', n.malCsrf.status === r.malCsrf.status && mismo(Object.keys(n.malCsrf.json || {}), Object.keys(r.malCsrf.json || {})), `${n.malCsrf.status} ${n.malCsrf.texto} vs ${r.malCsrf.status} ${r.malCsrf.texto}`)
    chequeo('csrf inválido: misma URL de error', (n.malCsrf.json || {}).url === (r.malCsrf.json || {}).url, `${(n.malCsrf.json || {}).url} vs ${(r.malCsrf.json || {}).url}`)
    chequeo('contraseña mala: mismo status', n.malaClave.status === r.malaClave.status, `${n.malaClave.status} vs ${r.malaClave.status}`)
    chequeo('contraseña mala: misma URL', (n.malaClave.json || {}).url === (r.malaClave.json || {}).url, `${(n.malaClave.json || {}).url} vs ${(r.malaClave.json || {}).url}`)
    chequeo('usuario inexistente: mismo status y URL', n.sinUsuario.status === r.sinUsuario.status && (n.sinUsuario.json || {}).url === (r.sinUsuario.json || {}).url, `${n.sinUsuario.status} ${JSON.stringify(n.sinUsuario.json)} vs ${r.sinUsuario.status} ${JSON.stringify(r.sinUsuario.json)}`)
    chequeo('login: mismo status', n.login.status === r.login.status, `${n.login.status} vs ${r.login.status}`)
    chequeo('login: misma URL de destino', (n.login.json || {}).url === (r.login.json || {}).url, `${(n.login.json || {}).url} vs ${(r.login.json || {}).url}`)
    chequeo('login: mismas cookies', nombresCookie(n.login.sc) === nombresCookie(r.login.sc), nombresCookie(n.login.sc) + ' vs ' + nombresCookie(r.login.sc))
    chequeo('login: mismos atributos de la cookie de sesión', atributos(n.login.sc, n.cookieSesion) === atributos(r.login.sc, r.cookieSesion), atributos(n.login.sc, n.cookieSesion) + ' vs ' + atributos(r.login.sc, r.cookieSesion))
    const quitaExp = (s) => { const c = JSON.parse(JSON.stringify(s || {})); delete c.expires; return c }
    chequeo('session tras login: mismo contenido', mismo(quitaExp(n.sesion.json), quitaExp(r.sesion.json)), JSON.stringify(n.sesion.json) + ' vs ' + JSON.stringify(r.sesion.json))
    chequeo('session: usuario correcto', r.sesion.json && r.sesion.json.user && r.sesion.json.user.id === usuario.id && r.sesion.json.user.role === 'PARTNER')
    chequeo('login sin json: 302 en ambos', n.loginHtml.status === 302 && r.loginHtml.status === 302, `${n.loginHtml.status} vs ${r.loginHtml.status}`)
    chequeo('login: callbackUrl externa se descarta (misma ubicación)', n.loginHtml.loc === r.loginHtml.loc, `${n.loginHtml.loc} vs ${r.loginHtml.loc}`)
    chequeo('signout: mismo status y URL', n.signout.status === r.signout.status && (n.signout.json || {}).url === (r.signout.json || {}).url, `${n.signout.status} ${JSON.stringify(n.signout.json)} vs ${r.signout.status} ${JSON.stringify(r.signout.json)}`)
    chequeo('signout: borra la cookie de sesión', r.signout.sc.some(s => s.startsWith(r.cookieSesion + '=;') || /max-age=0/i.test(s)), r.signout.sc.join(' | '))
    chequeo('session tras salir: {}', mismo(n.sesionTrasSalir.json, {}) && mismo(r.sesionTrasSalir.json, {}), JSON.stringify(n.sesionTrasSalir.json) + ' / ' + JSON.stringify(r.sesionTrasSalir.json))
    chequeo('signin → /login con callbackUrl', n.signinPagina.status === r.signinPagina.status && (n.signinPagina.loc || '').split('?')[0] === (r.signinPagina.loc || '').split('?')[0], `${n.signinPagina.status} ${n.signinPagina.loc} vs ${r.signinPagina.status} ${r.signinPagina.loc}`)
    chequeo('error → página de ingreso con el error', n.errorPagina.loc === r.errorPagina.loc, `${n.errorPagina.loc} vs ${r.errorPagina.loc}`)

    console.log('\n── intercambio de sesiones')
    const ck = (nombre, valor) => `${nombre}=${valor}`
    const conRust = await pedir(NEXT, Object.assign(new Jarra(), { c: new Map([[r.cookieSesion, r.valorSesion]]) }), 'GET', '/api/auth/session')
    chequeo('Next entiende la cookie emitida por Rust', conRust.json && conRust.json.user && conRust.json.user.id === usuario.id, JSON.stringify(conRust.json))
    const conNext = await pedir(RS, Object.assign(new Jarra(), { c: new Map([[n.cookieSesion, n.valorSesion]]) }), 'GET', '/api/auth/session')
    chequeo('Rust entiende la cookie emitida por Next', conNext.json && conNext.json.user && conNext.json.user.id === usuario.id, JSON.stringify(conNext.json))
    // Una ruta protegida de Rust con la cookie de Rust (usa el extractor Session)
    const protegida = await fetch(RS + '/api/users', { headers: { cookie: ck(r.cookieSesion, r.valorSesion) } })
    chequeo('ruta protegida de Rust acepta la cookie de Rust', protegida.status === 200 || protegida.status === 403, `status ${protegida.status}`)
  } finally {
    await prisma.sessionLog.deleteMany({ where: { OR: [{ email: CORREO }, { userId: usuario.id }] } }).catch(() => {})
    await prisma.user.delete({ where: { email: CORREO } }).catch(() => {})
    await prisma.$disconnect()
  }
  console.log(`\n══ RESUMEN auth: ${res.ok} ok · ${res.falla} fallas`)
  process.exit(res.falla ? 1 : 0)
})().catch(e => { console.error(e); process.exit(2) })
