
import { parseCsv, importCsv } from '../src/lib/api/csv.js';
const r = parseCsv('he said "hi", ok');
console.log("cells:", JSON.stringify(r.rows[0].cells.map(c=>c.value)));
console.log("import col0:", JSON.stringify(importCsv('he said "hi", ok').values.map(v=>v.value)));
