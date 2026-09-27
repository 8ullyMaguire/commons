/**
 * The field names a tagger can propose. Spec 10.10, plan T-P5-006 item 13;
 * #2318, #2399.
 *
 * # Why this list is here at all
 *
 * `FieldProposal.field` is a `String`, which is correct: §8.1's whole design is
 * that metadata is not a column per field, so a proposal can name a field this
 * build has never heard of and still be stored, voted on, and displayed. Adding
 * an enum would undo that.
 *
 * The cost of an open vocabulary is that a UI has nothing to offer as a list of
 * checkboxes. This module is that list, and it is a *starting point* rather than
 * a limit — `unknownEntries` in `ignore-list.js` reports an entry that matches
 * nothing here without removing it, because a scraper with a field this build
 * has not seen is normal and must still be silenceable.
 *
 * # It is derived from the schema, and a test pins it there
 *
 * Every name below is a real column or a real relation in
 * `crates/commons-core/src/domain.rs`. That is a claim that can rot, so
 * `tests/ignore-list.test.ts` reads the Rust source and asserts both directions:
 * every name here is a field the schema has, and every user-facing field the
 * schema has appears here. A struct gaining a field without this list gaining
 * it is a bug the test reports by name.
 *
 * The `*_id` and `created_at` / `updated_at` columns are deliberately absent —
 * see the note beside them.
 */

/**
 * Fields of an object, as a tagger sees them.
 *
 * `producer` rather than `producer_id`: the tagger proposes a NAME, and a user
 * reading a settings row does not care that the column is a foreign key. The
 * ignore list matches on what the user sees, because the user is the one
 * writing the list.
 */
export const OBJECT_FIELDS = [
  'title',
  'description',
  'date',
  'producer',
  'organized',
] as const;

/**
 * Fields of a performer.
 *
 * `aliases` is a list-valued field, and the tagger proposes it as a whole
 * rather than appending — see `splitByField`, which splits on the field name and
 * cannot half-apply a multi-valued field.
 */
export const PERFORMER_FIELDS = ['name', 'aliases', 'gender', 'nationality', 'birth_date', 'status'] as const;

/** Fields of a producer — studio, circle, individual or collective (§5.12). */
export const PRODUCER_FIELDS = [
  'name',
  'kind',
  'aliases',
  'urls',
  'defunct',
] as const;

/** Fields of a group — a release, series, compilation or franchise (§5.12). */
export const GROUP_FIELDS = ['name', 'description', 'code'] as const;

/** Fields of a tag. */
export const TAG_FIELDS = ['name', 'namespace', 'color', 'importance'] as const;

/**
 * Every field, grouped by subject, with the subject names as the tagger uses
 * them.
 *
 * The grouping is what makes a settings UI possible: "which of these do you
 * never want overwritten" is answerable per subject, and a flat list of thirty
 * names with no indication of what they belong to is not a question a user can
 * answer.
 */
export const FIELDS_BY_SUBJECT = {
  object: OBJECT_FIELDS,
  performer: PERFORMER_FIELDS,
  producer: PRODUCER_FIELDS,
  group: GROUP_FIELDS,
  tag: TAG_FIELDS,
} as const;

export type SubjectName = keyof typeof FIELDS_BY_SUBJECT;

/** Every known field name, across all subjects. */
export const KNOWN_TAGGER_FIELDS: readonly string[] = Object.values(FIELDS_BY_SUBJECT).flat();

/** The subjects, in a stable order for a settings UI. */
export const SUBJECTS: readonly SubjectName[] = Object.keys(FIELDS_BY_SUBJECT) as SubjectName[];

/**
 * Fields deliberately NOT offerable, per subject, and why.
 *
 * This is a list of omissions rather than a list of inclusions, because an
 * omission is the thing a reader cannot infer.
 *
 * It is keyed BY SUBJECT rather than being a flat list of names, and that is
 * not tidiness. `kind` is the discriminator on an `Object` — the seven object
 * kinds of §5.1 — and a tagger is invoked *on* an object, so its kind is
 * already decided before the tagger runs. It is also a real column on a
 * `Producer` (studio, circle, individual, collective, §5.12) where it is
 * metadata a tagger absolutely should propose. A flat name list cannot say both
 * of those things, so it has to pick one and be wrong.
 *
 * The per-subject omissions:
 *
 * - `id` on every subject — every subject has one, and a tagger does not
 *   propose it.
 * - `kind` on `object` — the §5.1 discriminator. A proposal for it would be a
 *   scraper changing what the item IS, which is a different operation from
 *   tagging and belongs to whatever moves a file between libraries.
 * - `created_at`, `updated_at` on every subject — timestamps of the row, not
 *   metadata. A proposal for `updated_at` would be a machine claiming it edited
 *   a row.
 * - `producer_id` on `group` — a foreign key to a producer that the tagger
 *   proposes by NAME through the producer's own fields. Ignoring `producer_id`
 *   would not stop a producer being attached, so the control would lie. This is
 *   why `group` offers `producer` and `object` offers `producer` too.
 * - `career_start`, `career_end` on `producer` — §7.11 says computed from item
 *   dates and never authored. A tagger that could overwrite them would be
 *   writing a derived value, which is the whole thing §7.11 forbids.
 * - `parent_id` on `tag` — §5.15's tree is structural, not metadata. It is set
 *   by a move, not by a scrape.
 * - `rating_sum`, `rating_count` on `object` — §8.8's ratings come from users,
 *   and the sum is derived; a proposal for either is a machine voting on a
 *   user's rating.
 */
export const NOT_OFFERABLE_BY_SUBJECT: Readonly<Record<SubjectName, readonly string[]>> = {
  object: [
    'id',
    'kind',
    'producer_id',
    'rating_sum',
    'rating_count',
    'created_at',
    'updated_at',
  ],
  performer: ['id', 'created_at', 'updated_at'],
  producer: [
    'id',
    'career_start',
    'career_end',
    'created_at',
    'updated_at',
  ],
  group: ['id', 'producer_id', 'created_at', 'updated_at'],
  tag: ['id', 'parent_id'],
};

/** Every not-offerable name across all subjects, for a membership test. */
export const NOT_OFFERABLE_FIELDS: readonly string[] = [
  ...new Set(Object.values(NOT_OFFERABLE_BY_SUBJECT).flat()),
];

/** Whether `field` is excluded from `subject`'s settings row. */
export function isOfferable(subject: SubjectName, field: string): boolean {
  return !NOT_OFFERABLE_BY_SUBJECT[subject].includes(field);
}
