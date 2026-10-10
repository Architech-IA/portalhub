// QA del CRM con usuarios y roles, en un navegador real. Crea una jefatura y dos vendedores con claves aleatorias (quedan en /root/crm-credenciales.txt, solo root).
const { chromium, request } = require('/root/pw/node_modules/playwright-core')
const fs = require('fs')
const crypto = require('crypto')
const URL = process.env.CRM_URL
const SHOTS = '/root/crm-shots2'
fs.mkdirSync(SHOTS, { recursive: true })
let fallos = 0
const ok = (c, m, x) => { if (!c) fallos++; console.log(`${c ? '  ok  ' : ' FALLA'} ${m}${!c && x !== undefined ? ' → ' + String(x).slice(0, 220) : ''}`) }
const texto = async (p) => (await p.locator('body').innerText()).replace(/\s+/g, ' ')
const clave = () => crypto.randomBytes(9).toString('base64url') + 'Aa1'
const suf = Date.now().toString().slice(-5)
const J = { nombre: 'Marta Quintero', email: 'marta@distribuidora-andina.example', clave: clave() }
const A = { nombre: 'Ana Vendedora', email: `ana${suf}@andina.example`, clave: clave() }
const L = { nombre: 'Luis Vendedor', email: `luis${suf}@andina.example`, clave: clave() }
fs.writeFileSync('/root/crm-credenciales.txt', `Jefatura: ${J.email} / ${J.clave}\nVendedora: ${A.email} / ${A.clave}\nVendedor: ${L.email} / ${L.clave}\n`, { mode: 0o600 })

;(async () => {
  const exe = fs.readdirSync('/root/.cache/ms-playwright').filter(d => d.startsWith('chromium-')).sort().pop()
  const browser = await chromium.launch({ executablePath: `/root/.cache/ms-playwright/${exe}/chrome-linux64/chrome`, args: ['--no-sandbox'] })
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const page = await ctx.newPage()
  const errores = []
  page.on('pageerror', e => errores.push(String(e)))
  page.on('console', m => { if (m.type() === 'error') errores.push(m.text()) })
  const ir = async (ruta) => { await page.goto(URL + ruta); await page.waitForLoadState('networkidle') }
  const enviar = async () => { await page.locator('form:has(input[name]:not([type=hidden])) button[type=submit]').first().click(); await page.waitForLoadState('networkidle'); await page.waitForTimeout(900) }
  const entrar = async (u) => { await ir('/login'); await page.fill('input[name=email]', u.email); await page.fill('input[name=clave]', u.clave); await enviar() }

  console.log('— Sin sesión')
  for (const r of ['/', '/empresas', '/oportunidades', '/actividades', '/usuarios']) { await ir(r); ok(page.url().endsWith('/login'), `${r} redirige a /login sin sesión`, page.url()) }

  console.log('— Jefatura inicial')
  await ir('/login')
  let t = await texto(page)
  ok(/Crear la jefatura inicial/.test(t), 'sin usuarios la página ofrece crear la jefatura inicial', t.slice(0, 160))
  const nombreAccion = await page.locator('input[type=hidden][name^="$ACTION_ID_"]').first().getAttribute('name')
  ok(!!nombreAccion, 'se pudo leer el identificador de la acción de alta inicial (para la prueba de ataque)', nombreAccion)
  await page.fill('input[name=nombre]', J.nombre); await page.fill('input[name=email]', J.email); await page.fill('input[name=clave]', 'corta')
  await enviar(); t = await texto(page)
  ok(page.url().endsWith('/login') && /8|caracteres|corta|mínim/i.test(t), 'una clave de menos de 8 caracteres se rechaza', t.slice(0, 200))
  await page.fill('input[name=nombre]', J.nombre); await page.fill('input[name=email]', J.email); await page.fill('input[name=clave]', J.clave)
  await enviar(); t = await texto(page)
  ok(!page.url().includes('/login') && t.includes(J.nombre), 'se crea la jefatura, inicia sesión y el menú muestra su nombre', page.url() + ' ' + t.slice(0, 120))
  const cookies = await ctx.cookies()
  const ck = cookies.find(c => c.name === 'sesion')
  ok(!!ck && ck.httpOnly === true, 'la cookie de sesión es httpOnly', JSON.stringify(ck))

  console.log('— Usuarios (jefatura)')
  await ir('/usuarios')
  ok(page.url().endsWith('/usuarios'), 'la jefatura entra a /usuarios')
  for (const u of [A, L]) {
    await page.fill('input[name=nombre]', u.nombre); await page.fill('input[name=email]', u.email); await page.selectOption('select[name=rol]', 'VENDEDOR'); await page.fill('input[name=clave]', u.clave)
    await enviar()
  }
  t = await texto(page)
  ok(t.includes(A.email) && t.includes(L.email), 'los dos vendedores aparecen en la lista de usuarios', t.slice(0, 260))

  console.log('— Ataque: crear una jefatura nueva con usuarios ya existentes (acción invocada directamente, sin sesión)')
  const intruso = { nombre: 'Intruso', email: `intruso${suf}@x.example`, clave: 'Intruso-12345' }
  const api = await request.newContext()
  const resp = await api.post(URL + '/login', { multipart: { [nombreAccion]: '', nombre: intruso.nombre, email: intruso.email, clave: intruso.clave }, maxRedirects: 0 }).catch(e => ({ status: () => 0, error: String(e) }))
  await api.dispose()
  await ir('/usuarios'); t = await texto(page)
  ok(!t.includes(intruso.email), 'la lista de usuarios NO contiene al intruso', t.slice(0, 220))
  const intr = await (await browser.newContext()).newPage()
  await intr.goto(URL + '/login'); await intr.fill('input[name=email]', intruso.email); await intr.fill('input[name=clave]', intruso.clave); await intr.click('button[type=submit]'); await intr.waitForLoadState('networkidle'); await intr.waitForTimeout(800)
  ok(intr.url().includes('/login'), 'el intruso no logra iniciar sesión con la cuenta que intentó crear', intr.url())

  console.log('— Datos de dos vendedores (como jefatura)')
  const crearEmpresa = async (nit, nombre, vendedor) => {
    await ir('/empresas'); await page.fill('input[name=nit]', nit); await page.fill('input[name=razonSocial]', nombre); await page.fill('input[name=ciudad]', 'Pereira'); await page.fill('input[name=vendedor]', vendedor); await enviar()
  }
  await crearEmpresa(`801${suf}-1`, `Empresa de Ana ${suf} SAS`, A.email)
  await crearEmpresa(`802${suf}-2`, `Empresa de Luis ${suf} SAS`, L.email)
  await ir('/empresas'); t = await texto(page)
  ok(t.includes(`Empresa de Ana ${suf}`) && t.includes(`Empresa de Luis ${suf}`), 'la jefatura ve las empresas de todos', t.slice(0, 200))
  const crearOpp = async (titulo, empresa, vendedor) => {
    await ir('/oportunidades'); await page.fill('input[name=titulo]', titulo)
    const ops = await page.locator('select[name=empresaId] option').allInnerTexts(); await page.selectOption('select[name=empresaId]', { index: ops.findIndex(o => o.includes(empresa)) })
    await page.fill('input[name=valor]', '2000000'); await page.fill('input[name=vendedor]', vendedor); await enviar()
  }
  await crearOpp(`Oportunidad de Ana ${suf}`, `Empresa de Ana ${suf}`, A.email)
  await crearOpp(`Oportunidad de Luis ${suf}`, `Empresa de Luis ${suf}`, L.email)
  await ir('/'); t = await texto(page)
  ok(/Oportunidades abiertas\s*2/.test(t), 'los KPIs de la jefatura cuentan las 2 oportunidades', t.slice(0, 240))
  await page.screenshot({ path: `${SHOTS}/1-jefatura-kpis.png`, fullPage: true })

  console.log('— Salir y entrar como vendedora')
  await page.getByRole('button', { name: /Salir/i }).click(); await page.waitForLoadState('networkidle'); await page.waitForTimeout(700)
  ok(page.url().endsWith('/login'), 'Salir cierra la sesión y vuelve al login', page.url())
  await ir('/empresas'); ok(page.url().endsWith('/login'), 'tras salir ya no se accede a /empresas', page.url())
  await entrar({ email: A.email, clave: 'clave-equivocada1' }); t = await texto(page)
  ok(page.url().endsWith('/login') && /incorrectos/i.test(t), 'una clave equivocada no entra', t.slice(0, 160))
  await entrar(A)
  ok(!page.url().includes('/login'), 'la vendedora entra con su clave', page.url())
  await ir('/empresas'); t = await texto(page)
  ok(t.includes(`Empresa de Ana ${suf}`) && !t.includes(`Empresa de Luis ${suf}`), 'la vendedora ve SOLO sus empresas', t.slice(0, 240))
  await ir('/oportunidades'); t = await texto(page)
  ok(t.includes(`Oportunidad de Ana ${suf}`) && !t.includes(`Oportunidad de Luis ${suf}`), 'la vendedora ve SOLO sus oportunidades', t.slice(0, 240))
  await ir('/'); t = await texto(page)
  ok(/Oportunidades abiertas\s*1\b/.test(t), 'los KPIs de la vendedora cuentan solo lo suyo (1)', t.slice(0, 240))
  await ir('/usuarios'); ok(!page.url().endsWith('/usuarios'), 'la vendedora no entra a /usuarios', page.url())
  await page.screenshot({ path: `${SHOTS}/2-vendedora.png`, fullPage: true })

  console.log('— Seguridad de la sesión')
  const cookieOk = (await ctx.cookies()).find(c => c.name === 'sesion')
  await ctx.addCookies([{ ...cookieOk, value: cookieOk.value.slice(0, -3) + 'abc' }])
  await ir('/empresas'); ok(page.url().endsWith('/login'), 'una cookie manipulada se rechaza', page.url())
  await ctx.addCookies([{ ...cookieOk, value: cookieOk.value.split('.')[0] + '.' }])
  await ir('/empresas'); ok(page.url().endsWith('/login'), 'una cookie sin firma se rechaza', page.url())

  console.log('— Celular (390 px)')
  const m = await (await browser.newContext({ viewport: { width: 390, height: 800 } })).newPage()
  await m.goto(URL + '/login'); await m.fill('input[name=email]', L.email); await m.fill('input[name=clave]', L.clave); await m.click('button[type=submit]'); await m.waitForLoadState('networkidle'); await m.waitForTimeout(800)
  for (const r of ['/', '/empresas', '/oportunidades', '/actividades']) {
    await m.goto(URL + r); await m.waitForLoadState('networkidle')
    const d = await m.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
    ok(d <= 4, `${r} no se desborda en 390 px (sobran ${d}px)`, d)
    if (r === '/oportunidades') await m.screenshot({ path: `${SHOTS}/3-movil-oportunidades.png`, fullPage: true })
  }
  ok(errores.length === 0, 'sin errores de consola ni de página en el escritorio', errores.slice(0, 3).join(' || '))
  await browser.close()
  console.log(fallos === 0 ? '\nTODO OK' : `\n${fallos} FALLA(S)`)
})().catch(e => { console.error('EXCEPCIÓN', e.message.slice(0, 400)); process.exit(1) })
