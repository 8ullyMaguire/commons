
import { scopeToQuery, ignoreEverything, scopeFrom } from '../src/lib/api/ignore-list.js';
console.log("inverted, no all:", JSON.stringify(scopeToQuery(ignoreEverything())));
console.log("inverted, with all:", JSON.stringify(scopeToQuery(ignoreEverything(), ['title','date'])));
console.log("empty scope:", JSON.stringify(scopeToQuery(scopeFrom([]))));
