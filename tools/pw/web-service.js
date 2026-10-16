// Navegación web para las sesiones del motor (portalhub). Solo escucha en 127.0.0.1.
// Playwright (Chromium headless) abre páginas y devuelve texto; nunca llega a direcciones internas del servidor.
const http = require('http')
const dns = require('dns').promises
const net = require('net')
const { chromium } = require('/root/pw/node_modules/playwright-core')
const fs = require('fs')
// Chromium ya instalado en el servidor (la versión que espera playwright-core no está descargada)
const RUTA_PW = '/root/.cache/ms-playwright'
const EXE = RUTA_PW + '/' + fs.readdirSync(RUTA_PW).filter(d => d.startsWith('chromium-')).sort().pop() + '/chrome-linux64/chrome'

const PORT = 3199
const MAX_SIMULTANEAS = 2
const MAX_TEXTO = 15000
const UA = 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36 ArchitechIA-Motor'

// ── Direcciones que jamás se visitan (red interna, metadatos de la nube, loopback…)
function v4Privada(ip) {
  const [a, b] = ip.split('.').map(Number)
  return a === 0 || a === 10 || a === 127 || a >= 224 || (a === 100 && b >= 64 && b <= 127) || (a === 169 && b === 254) ||
    (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 192 && b === 0) || (a === 198 && (b === 18 || b === 19))
}
function privada(ip) {
  if (net.isIPv4(ip)) return v4Privada(ip)
  const x = ip.toLowerCase()
  if (x === '::' || x === '::1') return true
  const m = x.match(/^::ffff:(\d+\.\d+\.\d+\.\d+)$/)
  if (m) return v4Privada(m[1])
  return x.startsWith('fc') || x.startsWith('fd') || /^fe[89ab]/.test(x) || x.startsWith('ff')
}
const cacheDns = new Map()
async function hostSeguro(host) {
  const h = host.replace(/^\[|\]$/g, '')
  if (net.isIP(h)) return !privada(h)
  if (h === 'localhost' || h.endsWith('.localhost') || h.endsWith('.internal') || h.endsWith('.local')) return false
  const c = cacheDns.get(h)
  if (c && Date.now() - c.t < 60000) return c.ok
  let ok = false
  try { const r = await dns.lookup(h, { all: true }); ok = r.length > 0 && r.every(x => !privada(x.address)) } catch { ok = false }
  cacheDns.set(h, { ok, t: Date.now() })
  return ok
}
async function urlSegura(u) {
  let x
  try { x = new URL(u) } catch { return false }
  if (x.protocol !== 'http:' && x.protocol !== 'https:') return false
  if (x.username || x.password) return false
  if (x.port && !['80', '443', '8080', '8443'].includes(x.port)) return false
  return hostSeguro(x.hostname)
}

// ── Un navegador compartido que se cierra solo cuando no se usa
let browserP = null, activos = 0, cola = [], idle = null
async function navegador() {
  if (!browserP) browserP = chromium.launch({ headless: true, executablePath: EXE, args: ['--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu'] }).catch(e => { browserP = null; throw e })
  return browserP
}
function programarCierre() {
  clearTimeout(idle)
  idle = setTimeout(async () => { if (activos === 0 && browserP) { const b = await browserP.catch(() => null); browserP = null; if (b) await b.close().catch(() => {}) } }, 60000)
}
async function turno() {
  if (activos < MAX_SIMULTANEAS) { activos++; return }
  await new Promise((ok, mal) => { const t = setTimeout(() => { cola = cola.filter(c => c !== f); mal(new Error('El navegador está ocupado; inténtalo en un momento')) }, 30000); const f = () => { clearTimeout(t); ok() }; cola.push(f) })
  activos++
}
function liberar() { activos--; const n = cola.shift(); if (n) n(); programarCierre() }

async function conPagina(fn) {
  await turno()
  let ctx
  try {
    const b = await navegador()
    ctx = await b.newContext({ userAgent: UA, locale: 'es-CO', viewport: { width: 1280, height: 900 }, acceptDownloads: false, serviceWorkers: 'block' })
    await ctx.route('**/*', async route => {
      const req = route.request()
      if (['image', 'media', 'font'].includes(req.resourceType())) return route.abort()
      if (!(await urlSegura(req.url()))) return route.abort()
      return route.continue()
    })
    const page = await ctx.newPage()
    page.setDefaultTimeout(20000)
    return await fn(page)
  } finally {
    if (ctx) await ctx.close().catch(() => {})
    liberar()
  }
}

// ── Impresión de documentos propios (HTML ya armado por el servidor: sin red, sin JavaScript) ──────────
async function conPaginaLocal(fn) {
  await turno()
  let ctx
  try {
    const b = await navegador()
    ctx = await b.newContext({ viewport: { width: 1000, height: 800 }, javaScriptEnabled: false, serviceWorkers: 'block', acceptDownloads: false })
    await ctx.route('**/*', r => { const u = r.request().url(); return (u.startsWith('data:') || u === 'about:blank') ? r.continue() : r.abort() })
    const page = await ctx.newPage()
    page.setDefaultTimeout(30000)
    return await fn(page)
  } finally {
    if (ctx) await ctx.close().catch(() => {})
    liberar()
  }
}
const aPdf = html => conPaginaLocal(async page => {
  await page.setContent(String(html || ''), { waitUntil: 'load' })
  const buf = await page.pdf({ format: 'A4', printBackground: true, margin: { top: '18mm', bottom: '18mm', left: '16mm', right: '16mm' } })
  return { base64: Buffer.from(buf).toString('base64') }
})
const aPng = html => conPaginaLocal(async page => {
  await page.setContent(String(html || ''), { waitUntil: 'load' })
  const el = await page.$('svg')
  const buf = el ? await el.screenshot({ type: 'png' }) : await page.screenshot({ type: 'png' })
  return { base64: Buffer.from(buf).toString('base64') }
})

const limpiar = t => t.replace(/[ \t]+/g, ' ').replace(/\n[ \t]+/g, '\n').replace(/\n{3,}/g, '\n\n').trim()

async function abrir(url) {
  if (!(await urlSegura(url))) throw new Error('Esa dirección no se puede visitar (solo sitios públicos http/https)')
  return conPagina(async page => {
    await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 20000 })
    await page.waitForTimeout(1200)
    const final = page.url()
    if (!(await urlSegura(final))) throw new Error('La página redirigió a una dirección no permitida')
    const datos = await page.evaluate(() => {
      document.querySelectorAll('script,style,noscript,svg,iframe,template').forEach(n => n.remove())
      const enlaces = [...document.querySelectorAll('a[href]')].map(a => ({ texto: (a.innerText || '').trim().replace(/\s+/g, ' ').slice(0, 80), href: a.href })).filter(a => a.texto && /^https?:/.test(a.href)).slice(0, 25)
      return { titulo: document.title || '', texto: document.body ? document.body.innerText : '', enlaces }
    })
    const texto = limpiar(datos.texto)
    return { titulo: datos.titulo, url: final, texto: texto.slice(0, MAX_TEXTO), truncado: texto.length > MAX_TEXTO, totalCaracteres: texto.length, enlaces: datos.enlaces }
  })
}

async function buscar(consulta) {
  const q = String(consulta || '').trim().slice(0, 200)
  if (!q) throw new Error('Falta la consulta')
  return conPagina(async page => {
    await page.goto('https://html.duckduckgo.com/html/?q=' + encodeURIComponent(q) + '&kl=es-es', { waitUntil: 'domcontentloaded', timeout: 20000 })
    const r = await page.evaluate(() => [...document.querySelectorAll('.result')].slice(0, 10).map(el => {
      const a = el.querySelector('a.result__a'); const s = el.querySelector('.result__snippet')
      return a ? { titulo: a.innerText.trim(), href: a.getAttribute('href') || '', fragmento: s ? s.innerText.trim() : '' } : null
    }).filter(Boolean))
    const resultados = r.map(x => {
      let u = x.href
      try { const p = new URL(u.startsWith('//') ? 'https:' + u : u); const real = p.searchParams.get('uddg'); if (real) u = real } catch { /* se deja tal cual */ }
      return { titulo: x.titulo.slice(0, 160), url: u, fragmento: x.fragmento.slice(0, 300) }
    }).filter(x => /^https?:\/\//.test(x.url))
    return { consulta: q, resultados, aviso: resultados.length ? undefined : 'El buscador no devolvió resultados (puede haber bloqueado la consulta). Prueba con otras palabras o abre directamente el sitio oficial.' }
  })
}

http.createServer((req, res) => {
  const fin = (c, o) => { res.writeHead(c, { 'content-type': 'application/json; charset=utf-8' }); res.end(JSON.stringify(o)) }
  if (req.method !== 'POST') return fin(405, { error: 'Método no permitido' })
  let cuerpo = ''
  req.on('data', d => { cuerpo += d; if (cuerpo.length > 3000000) req.destroy() })
  req.on('end', async () => {
    let j
    try { j = JSON.parse(cuerpo || '{}') } catch { return fin(400, { error: 'JSON inválido' }) }
    try {
      if (req.url === '/abrir') return fin(200, await abrir(String(j.url || '')))
      if (req.url === '/buscar') return fin(200, await buscar(j.consulta))
      if (req.url === '/pdf') return fin(200, await aPdf(j.html))
      if (req.url === '/png') return fin(200, await aPng(j.html))
      return fin(404, { error: 'No existe' })
    } catch (e) { return fin(200, { error: String(e && e.message ? e.message : e).split('\n')[0].slice(0, 300) }) }
  })
}).listen(PORT, '127.0.0.1', () => console.log('portalhub-web escuchando en 127.0.0.1:' + PORT))
