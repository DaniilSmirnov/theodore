// node _ts_parse.mjs FILE...: exit 0 when every TypeScript file parses.
// Syntax only: Node strips the types (module.stripTypeScriptTypes, Node
// >= 22.13) and checks the JavaScript left. No tsc, no node_modules.
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
import { execFileSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const dir = mkdtempSync(join(tmpdir(), 'ts-parse-'))
let bad = 0
for (const f of process.argv.slice(2)) {
  try {
    const js = stripTypeScriptTypes(readFileSync(f, 'utf8'), { mode: 'strip' })
    const out = join(dir, 'out.mjs')
    writeFileSync(out, js)
    execFileSync(process.execPath, ['--check', out], { stdio: 'pipe' })
  } catch (e) {
    bad++
    console.error(`${f}: ${String(e.stderr || e.message).split('\n').slice(0, 4).join(' ')}`)
  }
}
rmSync(dir, { recursive: true, force: true })
process.exit(bad ? 1 : 0)
