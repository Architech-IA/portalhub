// Abre una página de la aplicación que se está desarrollando (corre en un contenedor sin internet) con un navegador real
// y devuelve, en una línea JSON: estado HTTP, título, texto visible, errores de consola/página y solicitudes fallidas.
// Guarda una captura PNG. Solo se permiten solicitudes al mismo host de la aplicación.
const { chromium } = require('/root/pw/node_modules/playwright-core')
const fs = require('fs')
const [url, salida] = process.argv.slice(2)
const RUTA = '/root/.cache/ms-playwright'
const EXE = RUTA + '/' + fs.readdirSync(RUTA).filter(d => d.startsWith('chromium-')).sort().pop() + '/chrome-linux64/chrome'

;(async () => {
  const host = new URL(url).host
  const b = await chromium.launch({ headless: true, executablePath: EXE, args: ['--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu'] })
  try {
    const ctx = await b.newContext({ viewport: { width: 1280, height: 800 }, locale: 'es-CO', serviceWorkers: 'block' })
    await ctx.route('**/*', r => { const u = r.request().url(); let h = ''; try { h = new URL(u).host } catch { /* data: */ } return (h === host || u.startsWith('data:')) ? r.continue() : r.abort() })
    const page = await ctx.newPage()
    const consola = [], paginaErr = [], fallidas = []
    page.on('console', m => { if (m.type() === 'error') consola.push(m.text().slice(0, 200)) })
    page.on('pageerror', e => paginaErr.push(String((e && e.message) || e).slice(0, 200)))
    page.on('requestfailed', r => fallidas.push(r.url().slice(0, 120) + ' ' + ((r.failure() && r.failure().errorText) || '')))
    page.on('response', r => { if (r.status() >= 400) fallidas.push(r.status() + ' ' + r.url().slice(0, 120)) })
    let status = null
    try { const resp = await page.goto(url, { waitUntil: 'networkidle', timeout: 45000 }); status = resp ? resp.status() : null }
    catch { const resp = await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 20000 }); status = resp ? resp.status() : null }
    await page.waitForTimeout(800)
    const titulo = await page.title()
    const texto = await page.evaluate(() => (document.body ? document.body.innerText : ''))
    await page.screenshot({ path: salida, fullPage: false })
    console.log(JSON.stringify({ status, titulo, texto: texto.replace(/\n{3,}/g, '\n\n').slice(0, 3000), erroresConsola: consola.slice(0, 8), erroresPagina: paginaErr.slice(0, 8), solicitudesFallidas: fallidas.slice(0, 8) }))
  } finally { await b.close() }
})().catch(e => { console.error(String((e && e.message) || e).split('\n')[0]); process.exit(1) })
