"use strict";

// src/lib/api/csv.ts
var CANDIDATE_DELIMITERS = [",", ";", "	", "|"];
var SNIFF_LINES = 10;
function sniffDelimiter(text) {
  const lines = text.split(/\r\n|\r|\n/).filter((l) => l.trim() !== "").slice(0, SNIFF_LINES);
  if (lines.length === 0) return ",";
  let best = ",";
  let bestScore = -1;
  for (const candidate of CANDIDATE_DELIMITERS) {
    const counts = lines.map((line) => countOutsideQuotes(line, candidate));
    const nonZero = counts.filter((c) => c > 0);
    if (nonZero.length === 0) continue;
    const tally = /* @__PURE__ */ new Map();
    for (const c of nonZero) tally.set(c, (tally.get(c) ?? 0) + 1);
    let mode = 0;
    let modeSeen = 0;
    for (const [count, seen] of tally) {
      if (seen > modeSeen || seen === modeSeen && count > mode) {
        mode = count;
        modeSeen = seen;
      }
    }
    if (modeSeen > bestScore) {
      bestScore = modeSeen;
      best = candidate;
    }
  }
  return best;
}
function countOutsideQuotes(line, needle) {
  let inside = false;
  let n = 0;
  for (let i = 0; i < line.length; i += 1) {
    if (line[i] === '"') {
      if (inside && line[i + 1] === '"') {
        i += 1;
        continue;
      }
      inside = !inside;
      continue;
    }
    if (!inside && line.startsWith(needle, i)) {
      n += 1;
      i += needle.length - 1;
    }
  }
  return n;
}
function parseCsv(text, delimiter) {
  const delim = delimiter && delimiter !== "" ? delimiter : sniffDelimiter(text);
  const rows = [];
  const warnings = [];
  let truncatedAtLine = null;
  let blankLines = 0;
  let cells = [];
  let current = "";
  let cellQuoted = false;
  let inQuotes = false;
  let fieldStarted = false;
  let openedByQuote = false;
  let cellLine = 1;
  let rowLine = 1;
  let line = 1;
  const endField = () => {
    cells.push({ value: current, line: cellLine, quoted: openedByQuote || cellQuoted });
    current = "";
    cellQuoted = false;
    openedByQuote = false;
    fieldStarted = false;
  };
  const endRow = (isBlank) => {
    const isGap = isBlank && current === "" && !openedByQuote && !cellQuoted;
    endField();
    if (isGap && rows.length > 0) {
      cells = [];
      blankLines += 1;
      rowLine = line + 1;
      return;
    }
    rows.push({ cells, line: rowLine });
    cells = [];
    rowLine = line + 1;
  };
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (inQuotes) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          current += '"';
          i += 1;
          continue;
        }
        inQuotes = false;
        continue;
      }
      if (ch === "\r") {
        if (text[i + 1] === "\n") i += 1;
        current += "\n";
        line += 1;
        continue;
      }
      if (ch === "\n") {
        current += "\n";
        line += 1;
        continue;
      }
      current += ch;
      continue;
    }
    if (!fieldStarted) {
      if (ch === '"') {
        inQuotes = true;
        openedByQuote = true;
        fieldStarted = true;
        continue;
      }
      fieldStarted = true;
      if (ch === delim) {
        endField();
        cellLine = line;
        continue;
      }
      if (ch === "\r" || ch === "\n") {
        if (ch === "\r" && text[i + 1] === "\n") i += 1;
        endRow(current.trim() === "");
        line += 1;
        cellLine = line;
        rowLine = line;
        continue;
      }
      current += ch;
      continue;
    }
    if (text.startsWith(delim, i)) {
      endField();
      cellLine = line;
      i += delim.length - 1;
      continue;
    }
    if (ch === "\r" || ch === "\n") {
      if (ch === "\r" && text[i + 1] === "\n") i += 1;
      endRow(current.trim() === "");
      line += 1;
      cellLine = line;
      rowLine = line;
      continue;
    }
    if (ch === '"') {
      cellQuoted = true;
      warnings.push({
        line,
        message: `stray quote in an unquoted field; kept as data (row ${rowLine})`
      });
    }
    current += ch;
  }
  if (inQuotes) {
    truncatedAtLine = cellLine;
    endRow(false);
  } else if (fieldStarted || current !== "" || cells.length > 0) {
    endRow(false);
  }
  return { rows, delimiter: delim, truncatedAtLine, warnings, blankLines };
}
function importCsv(text, options = {}) {
  const parsed = parseCsv(text, options.delimiter ?? null);
  const { rows, delimiter, truncatedAtLine, warnings, blankLines } = parsed;
  const hasHeader = options.hasHeader === true;
  const headerRow = hasHeader ? rows[0] : void 0;
  const columnNames = headerRow ? headerRow.cells.map((c) => c.value) : [];
  const dataRows = hasHeader ? rows.slice(1) : rows;
  let index = 0;
  if (typeof options.column === "number") {
    index = options.column;
  } else if (typeof options.column === "string" && headerRow) {
    const found = columnNames.indexOf(options.column);
    index = found === -1 ? 0 : found;
  }
  const seen = new Set(options.existing ?? []);
  const values = [];
  let skipped = 0;
  let duplicates = 0;
  for (const row of dataRows) {
    const cell = row.cells[index];
    if (cell === void 0) {
      skipped += 1;
      continue;
    }
    const value = options.trimValues === true ? cell.value.trim() : cell.value;
    if (value === "" && !cell.quoted) {
      skipped += 1;
      continue;
    }
    if (seen.has(value)) {
      duplicates += 1;
      continue;
    }
    seen.add(value);
    values.push({ value, duplicate: false, line: cell.line });
  }
  return {
    values,
    skipped,
    duplicates,
    columnNames,
    delimiter,
    truncatedAtLine,
    warnings,
    rowCount: dataRows.length,
    blankLines
  };
}

// .probe/pc.ts
var r = parseCsv('he said "hi", ok');
console.log("cells:", JSON.stringify(r.rows[0].cells.map((c) => c.value)));
console.log("import col0:", JSON.stringify(importCsv('he said "hi", ok').values.map((v) => v.value)));
