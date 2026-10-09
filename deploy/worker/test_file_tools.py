"""
Pruebas de file_tools.py (herramientas de archivo del Motor Agentico).
    cd /opt/masd/worker && python3 -m unittest test_file_tools -v
No usan red ni Docker: run_command se prueba solo en su validacion (se reemplaza _correr_en_sandbox).
"""
import json
import os
import shutil
import tempfile
import unittest

import file_tools as ft


def _escribir(raiz, rel, texto):
    p = os.path.join(raiz, rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, 'w', encoding='utf-8', newline='') as fh:
        fh.write(texto)


def _leer(raiz, rel):
    with open(os.path.join(raiz, rel), 'r', encoding='utf-8', newline='') as fh:
        return fh.read()


class Base(unittest.TestCase):
    def setUp(self):
        self.raiz = os.path.realpath(tempfile.mkdtemp(prefix='ft-'))
        ft.set_repo_root(self.raiz)

    def tearDown(self):
        shutil.rmtree(self.raiz, ignore_errors=True)


class Lectura(Base):
    def test_archivo_corto_se_lee_entero(self):
        _escribir(self.raiz, 'src/a.ts', 'uno\ndos\n')
        self.assertEqual(ft.read_file('src/a.ts'), 'uno\ndos\n')

    def test_archivo_largo_avisa_cuantas_lineas_tiene(self):
        _escribir(self.raiz, 'src/grande.ts', ''.join(f'linea {i}\n' for i in range(1, 1001)))
        r = ft.read_file('src/grande.ts')
        self.assertIn('truncado', r)
        self.assertIn('1000 lineas', r)
        self.assertIn('start_line', r)

    def test_rango_de_lineas(self):
        _escribir(self.raiz, 'src/grande.ts', ''.join(f'linea {i}\n' for i in range(1, 1001)))
        r = ft.read_file('src/grande.ts', start_line=500, end_line=502)
        self.assertTrue(r.startswith('[src/grande.ts — lineas 500-502 de 1000]'))
        self.assertIn('linea 500\nlinea 501\nlinea 502\n', r)
        self.assertNotIn('linea 503', r)

    def test_rango_por_defecto_son_200_lineas(self):
        _escribir(self.raiz, 'src/g.ts', ''.join(f'l{i}\n' for i in range(1, 501)))
        r = ft.read_file('src/g.ts', start_line=10)
        self.assertIn('lineas 10-209 de 500', r)

    def test_rango_fuera_de_archivo_y_valores_invalidos(self):
        _escribir(self.raiz, 'src/a.ts', 'a\nb\n')
        self.assertIn('fuera de rango', ft.read_file('src/a.ts', start_line=50))
        self.assertIn('numeros enteros', ft.read_file('src/a.ts', start_line='x'))

    def test_fin_menor_que_inicio_no_rompe(self):
        _escribir(self.raiz, 'src/a.ts', 'a\nb\nc\n')
        self.assertIn('lineas 2-2 de 3', ft.read_file('src/a.ts', start_line=2, end_line=1))

    def test_se_puede_leer_package_json_y_esquema_pero_no_secretos(self):
        _escribir(self.raiz, 'package.json', '{"scripts": {"build": "tsc"}}')
        _escribir(self.raiz, 'prisma/schema.prisma', 'model A { id Int @id }')
        _escribir(self.raiz, '.env', 'SECRETO=1')
        _escribir(self.raiz, '.github/workflows/ci.yml', 'on: push')
        _escribir(self.raiz, 'ecosystem.config.js', 'env: {KEY: "x"}')
        self.assertIn('"build"', ft.read_file('package.json'))
        self.assertIn('model A', ft.read_file('prisma/schema.prisma'))
        for bloqueado in ('.env', '.github/workflows/ci.yml', 'ecosystem.config.js'):
            self.assertIn('bloqueada', ft.read_file(bloqueado), bloqueado)

    def test_no_sale_del_repositorio(self):
        with self.assertRaises(ft.FileToolError):
            ft.read_file('../../etc/passwd')
        self.assertIn('ERROR', ft.execute_tool('read_file', {'rel_path': '../../etc/passwd'}))

    def test_archivo_enorme_no_se_lee(self):
        _escribir(self.raiz, 'src/enorme.ts', 'x' * (ft.MAX_FILE_BYTES + 10))
        self.assertIn('pesa mas de', ft.read_file('src/enorme.ts'))


class Escritura(Base):
    def test_rutas_permitidas_nuevas(self):
        for rel in ('src/a.ts', 'app/page.tsx', 'tests/a.test.ts', 'docs/guia.md', 'prisma/migrations/001/migration.sql',
                    'README.md', 'Dockerfile', '.dockerignore', 'public/robots.txt', 'scripts/seed.ts'):
            self.assertTrue(ft.write_file(rel, 'x').startswith('OK'), rel)

    def test_rutas_no_permitidas(self):
        for rel in ('package.json', 'prisma/schema.prisma', 'next.config.js', '.env', '.env.local', '.git/config',
                    '.github/workflows/ci.yml', 'node_modules/x/index.js', '.masd-policy.json', 'otra/cosa.ts',
                    'package-lock.json', 'ecosystem.config.js'):
            self.assertIn('ERROR', ft.write_file(rel, 'x'), rel)
        self.assertFalse(os.path.exists(os.path.join(self.raiz, 'otra')))

    def test_no_escapa_con_rutas_absolutas_ni_dots(self):
        with self.assertRaises(ft.FileToolError):
            ft.write_file('src/../../fuera.ts', 'x')
        self.assertIn('ERROR', ft.execute_tool('write_file', {'rel_path': 'src/../../fuera.ts', 'content': 'x'}))
        self.assertFalse(os.path.exists(os.path.join(os.path.dirname(self.raiz), 'fuera.ts')))
        # una ruta absoluta se trata como relativa al repo: queda dentro, no en /
        self.assertTrue(ft.write_file('/src/dentro.ts', 'x').startswith('OK'))
        self.assertTrue(os.path.exists(os.path.join(self.raiz, 'src/dentro.ts')))

    def test_politica_del_repo_amplia_pero_no_desbloquea_secretos(self):
        _escribir(self.raiz, ft.POLICY_FILE, json.dumps({'escritura': ['prisma/schema.prisma', 'backend/', '.env', '.github/', '../x']}))
        self.assertTrue(ft.write_file('prisma/schema.prisma', 'model B {}').startswith('OK'))
        self.assertTrue(ft.write_file('backend/api.py', 'x').startswith('OK'))
        self.assertIn('ERROR', ft.write_file('.env', 'x'))
        self.assertIn('ERROR', ft.write_file('.github/workflows/ci.yml', 'x'))
        self.assertIn('ERROR', ft.write_file(ft.POLICY_FILE, '{}'))     # el agente no puede cambiar su propia politica
        self.assertIn('ERROR', ft.write_file('package.json', '{}'))      # package.json no esta en la politica de este repo

    def test_politica_rota_se_ignora(self):
        _escribir(self.raiz, ft.POLICY_FILE, '{no es json')
        self.assertIn('ERROR', ft.write_file('otra/cosa.ts', 'x'))
        self.assertTrue(ft.write_file('src/a.ts', 'x').startswith('OK'))

    def test_proteccion_contra_reescritura_truncada(self):
        grande = 'linea larga de codigo;\n' * 600     # ~13 KB
        _escribir(self.raiz, 'src/grande.ts', grande)
        r = ft.write_file('src/grande.ts', 'export {}\n')
        self.assertIn('reescritura truncada', r)
        self.assertEqual(_leer(self.raiz, 'src/grande.ts'), grande)              # no se tocó
        self.assertTrue(ft.write_file('src/grande.ts', 'export {}\n', reemplazar_completo=True).startswith('OK'))
        # un archivo chico sí se puede reescribir a algo más corto
        _escribir(self.raiz, 'src/chico.ts', 'abc' * 100)
        self.assertTrue(ft.write_file('src/chico.ts', 'a').startswith('OK'))


class Edicion(Base):
    def test_reemplazo_unico(self):
        _escribir(self.raiz, 'src/a.ts', 'export const a = 1\nexport const b = 2\n')
        r = ft.edit_file('src/a.ts', 'const b = 2', 'const b = 3')
        self.assertTrue(r.startswith('OK'), r)
        self.assertEqual(_leer(self.raiz, 'src/a.ts'), 'export const a = 1\nexport const b = 3\n')

    def test_no_encontrado_da_pista(self):
        _escribir(self.raiz, 'src/a.ts', 'hola\n')
        r = ft.edit_file('src/a.ts', 'chau', 'x')
        self.assertIn('no encontre', r)
        self.assertIn('read_file', r)

    def test_ambiguo_pide_contexto_o_replace_all(self):
        _escribir(self.raiz, 'src/a.ts', 'x = 1\nx = 1\nx = 1\n')
        r = ft.edit_file('src/a.ts', 'x = 1', 'x = 2')
        self.assertIn('3 veces', r)
        self.assertEqual(_leer(self.raiz, 'src/a.ts'), 'x = 1\nx = 1\nx = 1\n')
        self.assertIn('3 reemplazo', ft.edit_file('src/a.ts', 'x = 1', 'x = 2', replace_all=True))
        self.assertEqual(_leer(self.raiz, 'src/a.ts'), 'x = 2\nx = 2\nx = 2\n')

    def test_conserva_saltos_de_linea_crlf(self):
        _escribir(self.raiz, 'src/w.ts', 'a\r\nb\r\nc\r\n')
        r = ft.edit_file('src/w.ts', 'a\nb', 'a\nB')
        self.assertTrue(r.startswith('OK'), r)
        self.assertEqual(_leer(self.raiz, 'src/w.ts'), 'a\r\nB\r\nc\r\n')

    def test_archivo_grande_editado_en_el_medio_sin_perder_el_resto(self):
        lineas = [f'export function f{i}() {{ return {i} }}\n' for i in range(1, 801)]      # ~34 KB, > 6000 chars
        _escribir(self.raiz, 'src/lib.ts', ''.join(lineas))
        # el agente lee solo un trozo del medio y lo cambia
        trozo = ft.read_file('src/lib.ts', start_line=400, end_line=401)
        self.assertIn('f400', trozo)
        r = ft.edit_file('src/lib.ts', 'export function f400() { return 400 }', 'export function f400() { return 4000 }')
        self.assertTrue(r.startswith('OK'), r)
        nuevo = _leer(self.raiz, 'src/lib.ts')
        self.assertEqual(len(nuevo.splitlines()), 800)
        self.assertIn('return 4000', nuevo)
        self.assertIn('f1()', nuevo)
        self.assertIn('f800()', nuevo)

    def test_validaciones(self):
        _escribir(self.raiz, 'src/a.ts', 'hola\n')
        self.assertIn('no puede estar vacio', ft.edit_file('src/a.ts', '', 'x'))
        self.assertIn('iguales', ft.edit_file('src/a.ts', 'hola', 'hola'))
        self.assertIn('no existe', ft.edit_file('src/noexiste.ts', 'a', 'b'))
        _escribir(self.raiz, 'package.json', '{}')
        self.assertIn('bloqueada', ft.edit_file('package.json', '{}', '{"a":1}'))
        self.assertEqual(_leer(self.raiz, 'package.json'), '{}')
        _escribir(self.raiz, '.env', 'A=1')
        self.assertIn('bloqueada', ft.edit_file('.env', 'A=1', 'A=2'))

    def test_via_execute_tool(self):
        _escribir(self.raiz, 'src/a.ts', 'uno\n')
        self.assertTrue(ft.execute_tool('edit_file', {'rel_path': 'src/a.ts', 'old_text': 'uno', 'new_text': 'dos'}).startswith('OK'))
        self.assertIn('falta el argumento', ft.execute_tool('edit_file', {'rel_path': 'src/a.ts'}))


class BorrarYMover(Base):
    def test_borrar(self):
        _escribir(self.raiz, 'src/a.ts', 'x')
        self.assertTrue(ft.delete_file('src/a.ts').startswith('OK'))
        self.assertFalse(os.path.exists(os.path.join(self.raiz, 'src/a.ts')))
        self.assertIn('no existe', ft.delete_file('src/a.ts'))
        _escribir(self.raiz, 'package.json', '{}')
        self.assertIn('bloqueado', ft.delete_file('package.json'))
        self.assertTrue(os.path.exists(os.path.join(self.raiz, 'package.json')))

    def test_mover(self):
        _escribir(self.raiz, 'src/viejo.ts', 'x')
        self.assertTrue(ft.move_file('src/viejo.ts', 'src/nuevo/dir/nuevo.ts').startswith('OK'))
        self.assertTrue(os.path.exists(os.path.join(self.raiz, 'src/nuevo/dir/nuevo.ts')))
        _escribir(self.raiz, 'src/otro.ts', 'y')
        self.assertIn('ya existe', ft.move_file('src/otro.ts', 'src/nuevo/dir/nuevo.ts'))
        self.assertIn('bloqueado', ft.move_file('src/otro.ts', '.env'))
        self.assertIn('bloqueado', ft.move_file('package.json', 'src/p.json'))


class Grep(Base):
    def test_busca_en_mas_tipos_y_excluye_node_modules(self):
        _escribir(self.raiz, 'prisma/schema.prisma', 'model Cliente { id Int }')
        _escribir(self.raiz, 'db/seed.sql', 'insert into cliente values (1)')
        _escribir(self.raiz, 'node_modules/x/index.js', 'Cliente en dependencia')
        r = ft.grep_files('liente')
        self.assertIn('prisma/schema.prisma', r)
        self.assertIn('db/seed.sql', r)
        self.assertNotIn('node_modules', r)


class Comandos(Base):
    def setUp(self):
        super().setUp()
        self.ejecutados = []
        self._orig = ft._correr_en_sandbox

        class R:
            returncode = 0
            stdout = 'ok'
            stderr = ''
        ft._correr_en_sandbox = lambda comando, timeout: (self.ejecutados.append(comando), R())[1]

    def tearDown(self):
        ft._correr_en_sandbox = self._orig
        super().tearDown()

    def test_prisma_solo_validate_format_generate(self):
        for sub in ('validate', 'format', 'generate'):
            self.assertTrue(ft.run_command(['npx', 'prisma', sub]).startswith('[exit 0]'), sub)
        for sub in ('migrate', 'db', 'studio', 'init'):
            self.assertIn('ERROR', ft.run_command(['npx', 'prisma', sub] + (['push'] if sub == 'db' else [])), sub)
        self.assertIn('ERROR', ft.run_command(['npx', 'prisma']))
        self.assertEqual(len(self.ejecutados), 3)

    def test_lo_que_ya_estaba_prohibido_sigue_prohibido(self):
        for cmd in (['rm', '-rf', '/'], ['npx', 'cualquier-paquete'], ['npm', 'run', 'dev'], ['npm', 'install', '/etc/passwd'],
                    ['npm', 'install', '../x'], ['npx', 'tsc', '--project', '/tmp/x']):
            self.assertIn('ERROR', ft.run_command(cmd), cmd)
        self.assertEqual(self.ejecutados, [])

    def test_esquemas_de_herramientas_consistentes(self):
        nombres = [t['function']['name'] for t in ft.TOOL_SCHEMAS]
        for esperado in ('list_files', 'read_file', 'grep_files', 'write_file', 'edit_file', 'delete_file', 'move_file', 'run_command'):
            self.assertIn(esperado, nombres)
        self.assertEqual(len(nombres), len(set(nombres)))
        for t in ft.TOOL_SCHEMAS:
            f = t['function']
            for requerido in f['parameters'].get('required', []):
                self.assertIn(requerido, f['parameters']['properties'], f['name'])


if __name__ == '__main__':
    unittest.main(verbosity=2)
