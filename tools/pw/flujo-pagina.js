// Ejecuta un flujo de varios pasos (ir, clic, escribir, esperar, verificar…) sobre la aplicación que se está desarrollando
// (corre en un contenedor sin internet) con un navegador real. Los pasos llegan en la variable FLUJO (JSON).
// Solo se permiten solicitudes al mismo host de la aplicación. Escribe una captura por captura pedida y otra del paso que falle.
const { chromium } = require('/root/pw/node_modules/playwright-core')
const fs = require('fs')
const [base, dir, prefijo] = process.argv.slice(2)
const RUTA = '/root/.cache/ms-playwright'
const EXE = RUTA + '/' + fs.readdirSync(RUTA).filter(d => d.startsWith('chromium-')).sort().pop() + '/chrome-linux64/chrome'
const pasos = JSON.parse(process.env.FLUJO || '[]')

;(async () => {
  const host = new URL(base).host
  const b = await chromium.launch({ headless: true, executablePath: EXE, args: ['--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu'] })
  const resultados = [], capturas = [], consola = []
  try {
    const ctx = await b.newContext({ viewport: { width: 1280, height: 800 }, locale: 'es-CO', serviceWorkers: 'block' })
    await ctx.route('**/*', r => { const u = r.request().url(); let h = ''; try { h = new URL(u).host } catch { /* data: */ } return (h === host || u.startsWith('data:')) ? r.continue() : r.abort() })
    const page = await ctx.newPage()
    page.setDefaultTimeout(10000)
    page.on('console', m => { if (m.type() === 'error') consola.push(m.text().slice(0, 160)) })
    page.on('pageerror', e => consola.push('Error de la página: ' + String((e && e.message) || e).slice(0, 160)))
    const captura = async n => { const f = `${dir}/${prefijo}-${n}.png`; await page.screenshot({ path: f }); capturas.push(f.split('/').pop()); return f }
    let n = 0
    for (const p of pasos) {
      n++
      const r = { n, paso: p.accion, ok: true, detalle: '' }
      try {
        switch (p.accion) {
          case 'ir': await page.goto(base + p.ruta, { waitUntil: 'domcontentloaded', timeout: 30000 }); await page.waitForTimeout(500); r.detalle = 'HTTP ok · ' + page.url().replace(base, '') || '/'; break
          case 'clic': await page.click(p.selector); await page.waitForTimeout(400); r.detalle = 'clic en ' + p.selector; break
          case 'escribir': await page.fill(p.selector, String(p.valor ?? '')); r.detalle = 'escrito en ' + p.selector; break
          case 'seleccionar': await page.selectOption(p.selector, String(p.valor ?? '')); r.detalle = 'seleccionado ' + p.valor; break
          case 'esperar_texto': await page.getByText(String(p.texto), { exact: false }).first().waitFor({ state: 'visible' }); r.detalle = 'apareció «' + p.texto + '»'; break
          case 'esperar_selector': await page.waitForSelector(p.selector, { state: 'visible' }); r.detalle = 'visible ' + p.selector; break
          case 'verificar_texto': { const t = await page.evaluate(() => document.body.innerText); if (!t.toLowerCase().includes(String(p.texto).toLowerCase())) throw new Error('no aparece el texto «' + p.texto + '». Texto visible: ' + t.replace(/\s+/g, ' ').slice(0, 220)); r.detalle = 'el texto «' + p.texto + '» está en la página'; break }
          case 'verificar_url': { const u = page.url().replace(base, ''); if (!u.includes(String(p.contiene))) throw new Error('la URL es «' + u + '» y no contiene «' + p.contiene + '»'); r.detalle = 'URL ' + u; break }
          case 'verificar_visible': { if (!(await page.isVisible(p.selector))) throw new Error('no está visible ' + p.selector); r.detalle = 'visible ' + p.selector; break }
          case 'capturar': await captura(n); r.detalle = 'captura guardada'; break
          default: throw new Error('paso desconocido')
        }
      } catch (e) {
        r.ok = false; r.detalle = String((e && e.message) || e).split('\n')[0].slice(0, 300)
        try { await captura('fallo' + n) } catch { /* sin captura */ }
        resultados.push(r)
        break
      }
      resultados.push(r)
    }
    console.log(JSON.stringify({ resultados, capturas, urlFinal: page.url().replace(base, '') || '/', erroresConsola: consola.slice(0, 6), total: pasos.length }))
  } finally { await b.close() }
})().catch(e => { console.error(String((e && e.message) || e).split('\n')[0]); process.exit(1) })
