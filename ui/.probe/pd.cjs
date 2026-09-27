"use strict";

// src/lib/api/tagger-fields.ts
var OBJECT_FIELDS = [
  "title",
  "description",
  "date",
  "producer",
  "organized"
];
var PERFORMER_FIELDS = ["name", "aliases", "gender", "nationality", "birth_date", "status"];
var PRODUCER_FIELDS = [
  "name",
  "kind",
  "aliases",
  "urls",
  "defunct"
];
var GROUP_FIELDS = ["name", "description", "code"];
var TAG_FIELDS = ["name", "namespace", "color", "importance"];
var FIELDS_BY_SUBJECT = {
  object: OBJECT_FIELDS,
  performer: PERFORMER_FIELDS,
  producer: PRODUCER_FIELDS,
  group: GROUP_FIELDS,
  tag: TAG_FIELDS
};
var KNOWN_TAGGER_FIELDS = Object.values(FIELDS_BY_SUBJECT).flat();
var SUBJECTS = Object.keys(FIELDS_BY_SUBJECT);

// src/lib/api/ignore-list.ts
function normaliseField(field) {
  return field.trim().toLowerCase();
}
function isValidFieldName(field) {
  if (field === "") return false;
  return /^[a-z0-9_.-]+$/.test(normaliseField(field));
}
function ignoreEverything() {
  return { ignoreAll: true, fields: [] };
}
function normaliseAll(fields) {
  const set = /* @__PURE__ */ new Set();
  for (const f of fields) {
    const n = normaliseField(f);
    if (n !== "") set.add(n);
  }
  return [...set].sort();
}
function isIgnored(scope, field) {
  const n = normaliseField(field);
  if (n === "") return false;
  return scope.ignoreAll ? !scope.fields.includes(n) : scope.fields.includes(n);
}
function keptFields(scope, all) {
  return all.filter((f) => !isIgnored(scope, f));
}
function scopeFrom(fields, ignoreAll = false) {
  return {
    ignoreAll,
    fields: normaliseAll(fields.filter(isValidFieldName))
  };
}
function scopeToQuery(scope, all) {
  if (!scope.ignoreAll) {
    return scope.fields.map((f) => `ignore=${encodeURIComponent(f)}`).join("&");
  }
  const keep = all === void 0 ? scope.fields : keptFields(scope, all);
  return keep.map((f) => `keep=${encodeURIComponent(f)}`).join("&");
}

// .probe/pd.ts
console.log("inverted, no all:", JSON.stringify(scopeToQuery(ignoreEverything())));
console.log("inverted, with all:", JSON.stringify(scopeToQuery(ignoreEverything(), ["title", "date"])));
console.log("empty scope:", JSON.stringify(scopeToQuery(scopeFrom([]))));
