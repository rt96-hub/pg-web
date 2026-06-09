# 007 — Tera Render Errors, Boolean Leakage in json_build_object, and Pain with Complex List Handlers

**Status:** Ready for framework discussion / improvement  
**Priority:** High (developer experience blocker)  
**Related to:** Building non-trivial list pages with dynamic columns (especially when adding research/workspace data on top of existing complex keyset pagination)

## The Problem We Hit (May 2026, trucking-carriers project)

While implementing prompt 004 (adding research workspace fields — pipeline state, flags, notes, contacts — to the main carriers list and detail views), we repeatedly ran into a very painful class of bugs:

### Symptom 1: Mysterious "Test 'true' not found" Tera errors

Error in logs (even when the page partially rendered):
```
PGWEB_E007_TEMPLATE_RENDER: pages/carriers/index.html — Failed to render '__tera_one_off' → Test 'true' not found
```

This happened on every request to `/carriers`.

**Root cause:** Defensive template code of this form:

```tera
{% if c.pipeline and c.pipeline is not true and c.pipeline is not false %}
    {{ c.pipeline }}
{% else %}
    —
{% endif %}
```

Tera does **not** support `is true` / `is not true` / `is false` tests (unlike some Jinja2 configurations or Django templates). It treats `true` as an unknown test name.

### Symptom 2: Per-row columns rendering the literal word "true" (boolean leakage)

Even after "fixing" the template syntax, the Pipeline and Flag columns in the main table would render the word `true` (as text) instead of the actual research values or `—`.

This happened:
- For carriers with **no** research data.
- For carriers that **did** have real seeded data (e.g. DOT 3752318 with `pipeline = 'Qualified'` and `flag = 'HighPotential'`).

The filter dropdowns (populated from distinct values in the research tables) worked correctly, proving the data existed and the top-level response was recent. But the per-carrier rows in the `carriers` array were polluted with boolean `true`.

### Why This Was So Hard to Debug

The carriers list handler (`pages/carriers/index.sql`) is ~500 lines of dense PL/pgSQL implementing:
- Generalized keyset (cursor) pagination
- Multiple sort directions
- Forward + backward pagination
- Numbered page jumps (which use OFFSET once then keyset)
- Dynamic `ORDER BY` expressions
- Complex `WHERE` building for search + filters
- Multiple nearly-identical `json_agg(json_build_object(...))` blocks (at least 5–6 different code paths)

When we added two new research columns (`pipeline` and `flag`), we had to inject subqueries into **every** one of those json_build_object sites:

```sql
'pipeline', COALESCE((SELECT state FROM public.carrier_pipeline_state WHERE dot_number = c.dot_number LIMIT 1), ''),
'flag',     COALESCE((SELECT flag  FROM public.carrier_flags        WHERE dot_number = c.dot_number LIMIT 1), ''),
```

Different branches had slightly different aliasing (`c`, `prev`, bare columns from nested derived tables). During iterative fixes, some replaces left:
- Double-wrapped COALESCE
- Inconsistent correlation (`dot_number = dot_number` vs `c.dot_number`)
- Leftover expressions from earlier debugging attempts

Because `pg-web dev` does not always reliably hot-reload complex handlers + templates (especially after many iterations), we would push, see "it works on page 1", click Next, and get boolean `true` again from a different code path that hadn't been updated or reloaded.

The error messages from Tera were not helpful ("Test 'true' not found" instead of pointing at the exact expression or column that became boolean).

## Contributing Factors

- **No easy way to see the exact JSON context** a template receives for a given row (short of adding temporary debug columns or using browser dev tools on a live request).
- **Dynamic SQL + json_build_object** makes it very easy to accidentally produce boolean values when expressions are complex or when variables from the outer PL/pgSQL function (`v_flag`, `v_pipeline`, filter conditions, etc.) leak into the wrong scope during editing.
- **Tera's limited boolean testing** compared to what many developers expect from other templating systems.
- **Reload friction** in development for pages with many similar but not identical query construction paths.
- The existing keyset pagination logic was already extremely complex before research fields were added (prompts 001 + 003). Adding per-row research data turned it into a maintenance nightmare.

## Recommendations for pg-web

1. **Better debugging for template context**
   - A dev-only mode or header that dumps the exact JSON context passed to Tera (or at least the shape for list rows).
   - Or a `pg-web debug template` command that shows what variables are available.

2. **Stricter / safer json_build_object patterns**
   - Guidance or helpers that encourage always using `COALESCE(..., '')` or explicit casting for display columns.
   - Possibly runtime warnings in dev when a json_build_object value is boolean in a context where a string/number was expected.

3. **Improved Tera error messages**
   - When an unknown test (`is true`, etc.) is used, point to the exact line + suggest the supported alternatives (`== true`, truthiness, `is defined`, `is null`, etc.).

4. **Hot reload reliability for complex handlers**
   - Better detection / forced reload of functions when `.sql` files under `pages/` change significantly.
   - Or a `--force-reload` flag for dev.

5. **Documentation**
   - Explicit section in APP-DEVELOPER-GUIDE.md warning about boolean leakage when mixing filter logic variables with row data in dynamic queries.
   - Recommended patterns for adding "extra columns" to existing complex list handlers.

6. **Future**
   - Consider a higher-level helper or view layer for common "list + dynamic research columns" use cases so app developers don't have to duplicate subqueries across 6 pagination branches.

## How This Manifested in Practice (trucking-carriers)

We were trying to turn a basic FMCSA viewer into a lightweight research workspace. Adding two simple per-carrier fields (current pipeline state + current flag) to an already-sophisticated list view triggered days of lost time on boolean `true` rendering and opaque Tera errors.

The filter + options parts worked. The per-row data in the table did not. The combination of a giant handler + dynamic SQL + Tera's strictness + reload quirks made root-causing extremely slow.

This feels like the kind of "paper cut" that will repeatedly slow down anyone building anything beyond the simplest CRUD lists on top of pg-web.

---

**Definition of Done for this prompt (if turned into framework work)**

- Clear documentation + recommended patterns for adding research/extra columns to list handlers.
- Improved error messages or debugging tools that would have made "where is this boolean coming from?" obvious within minutes instead of hours.
- At least one concrete improvement (better reload, context dumping, or Tera test sugar) that would have prevented or quickly surfaced the "Test 'true' not found" + boolean leakage class of bugs.

This issue was encountered while executing prompt 004 in the trucking-carriers repository (May 2026). It is recorded here as high-signal feedback for the pg-web framework.