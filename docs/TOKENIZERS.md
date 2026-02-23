# Tokenizer Pipeline Reference

This document describes the custom tokenizers in tantivy_rb in detail. These tokenizers were ported from the Java PatentSafe search analysers to produce identical token output, ensuring that documents indexed by the Rust code are searchable with the same queries that worked in the Java application.

## Why custom tokenizers?

PatentSafe indexes technical and scientific documents that contain a mix of natural language, chemical formulae, part numbers, DNA sequences, dates, and other structured identifiers. A standard full-text tokenizer would either:

- Break `"E21634-016"` into `["E21634", "016"]` and lose the ability to search for the full ID
- Keep `"E21634-016"` as a single token and lose the ability to search for just `"016"`

The compound tokenizer solves this by **classifying** each token and applying different strategies: plain words get stemmed, while complex tokens (containing mixed letters, digits, and punctuation) get expanded into n-gram sub-spans so that both the full form and all meaningful sub-parts are searchable.

## Tokenizer types

### `:default` — Standard pipeline

**Source:** `tokenizer/default.rs`

A conventional text analysis pipeline using Tantivy's built-in filters:

```
Input text
  │
  ├─ WhitespaceTokenizer    split on whitespace
  ├─ AsciiFoldingFilter     é → e, ü → u, etc.
  ├─ LowerCaser             HELLO → hello
  ├─ StopWordFilter         remove "the", "and", "is", etc.
  └─ Stemmer                "running" → "run"
```

Good for general-purpose text fields. Not used by PatentSafe's search — listed here for completeness.

### `:raw` — No tokenization

The entire field value is stored as a single token, with no transformation. Used for exact-match fields like document IDs and filter facets (state, document type, author name).

### `:compound` — PatentSafe compound tokenizer

The core tokenizer, with two modes that share classification logic but differ in what they emit.

---

## Compound index tokenizer

**Source:** `tokenizer/compound/mod.rs`
**Java equivalent:** `FullIndexingAnalyser`
**Registered as:** `type: :compound, mode: :index`

### Pipeline overview

```
Input text
  │
  ├─ 1. Whitespace split
  │
  ├─ 2. Strip leading/trailing punctuation (configurable char sets)
  │
  ├─ 3. Classify token as WORD, COMPLEX, or Skip
  │
  ├─ 4. ASCII fold (é → e)
  │
  ├─ 5. Lowercase
  │
  ├─ 6a. [WORD path]                    6b. [COMPLEX path]
  │     │                                    │
  │     ├─ Stop word check                   └─ N-gram expansion
  │     │  (skip if stop word)                  (generate sub-spans)
  │     │
  │     └─ Stem + dual-emit
  │        (stemmed form + original
  │         at same position)
  │
  └─ Output token stream
```

### Step-by-step detail

#### 1. Whitespace split

The input is split on Unicode whitespace into raw tokens. Each raw token is processed independently.

#### 2. Punctuation stripping

Configurable leading and trailing character sets control what gets stripped. The defaults match the Java `BlockTokenParsingFilter`:

| Direction | Characters | Purpose |
|-----------|-----------|---------|
| Leading   | `. , : ; " ) > < } ] ~ +` | Strip opening punctuation that isn't part of the token |
| Trailing  | `. , : ; " ( < > [ { %`   | Strip closing punctuation |

Note the asymmetry: `)` is stripped from the front, `(` from the back. This means `"(Fred)"` keeps both parentheses (since `(` is not in the leading set and `)` is not in the trailing set), making it a COMPLEX token that gets expanded.

#### 3. Token classification

**Source:** `tokenizer/compound/classifier.rs`

After stripping, each token is classified:

| Classification | Rule | Examples | Treatment |
|---------------|------|----------|-----------|
| **WORD** | All characters are Unicode letters | `Hello`, `café`, `JJPD` | Stemmed + stop-word filtered |
| **COMPLEX** | Mixed characters with at least one letter or digit | `E21634-016`, `C11.20`, `09/VPAC14/MB02` | N-gram expanded |
| **Skip** | Pure punctuation/symbols, or empty after stripping | `---`, `...`, `===` | Dropped entirely |

The key insight: any token containing a mix of letters, digits, and punctuation is COMPLEX. This catches part numbers, chemical identifiers, dates, phone numbers, DNA sequences, and other structured data.

#### 4-5. ASCII folding and lowercasing

Standard normalization applied to all non-skipped tokens. The ASCII folder handles common Latin-1 accented characters (à→a, ñ→n, ü→u, etc.).

#### 6a. WORD path — stemming with dual-emission

For WORD tokens, the pipeline:

1. Checks against the stop word list — if it's a stop word (e.g. "the", "and"), skip it entirely
2. Stems the token using the Snowball algorithm (e.g. "running" → "run")
3. Emits the **stemmed form** at the next position
4. If the stemmed form differs from the lowercased original, **also emits the original** at the **same position**

Same-position tokens act as **synonyms** in Tantivy — the query engine treats them as OR alternatives. This dual-emission matches the Java `FullIndexingAnalyser` behaviour and provides two benefits:

- **Recall:** Searching for "run" matches documents containing "running" (via the stemmed form)
- **Precision:** Searching for "running" gets a BM25 boost on documents that literally contain "running" (matching both the stemmed AND the original token), ranking them higher than documents that only contain "run"

**Example:** `"running experiments"`

```
Position 1: "run"          (stemmed)
Position 1: "running"      (original, same position = synonym)
Position 2: "experi"       (stemmed)
Position 2: "experiments"  (original, same position = synonym)
```

#### 6b. COMPLEX path — n-gram expansion

**Source:** `tokenizer/compound/expander.rs`

For COMPLEX tokens, the pipeline generates sub-span combinations:

1. Parse the token into **character-type blocks** — contiguous runs of the same type (LETTER, NUMBER, OTHER). OTHER characters always form their own block boundary.
2. Emit the **full token** first
3. Generate sub-spans by combining consecutive blocks, starting from each block position

All sub-spans are emitted at the **same position** as the full token, so they act as synonyms — searching for any sub-span matches the document.

**Example:** `"E21634-016"`

Blocks: `[E]` `[21634]` `[-]` `[016]`

Output (all at the same position):
- `e21634-016` (full token)
- `e` (single block)
- `e21634` (blocks 0-1)
- `e21634-` (blocks 0-2)
- `e21634-016` (blocks 0-3, skipped as duplicate of full)
- `21634` (single block)
- `21634-` (blocks 1-2)
- `21634-016` (blocks 1-3)
- `-` (single block, but OTHER-only → skipped by validity check)
- `-016` (blocks 2-3, valid because it contains "016")
- `016` (single block)

**Safety bounds:** To prevent exponential expansion on pathological inputs (e.g. long DNA sequences with many dash-separated segments), the expander enforces:
- `MAX_TOKEN_LENGTH = 100` — sub-spans longer than 100 characters are skipped
- `MAX_TOKEN_BLOCKS = 45` — at most 45 consecutive blocks combined from any starting position

### Position numbering

Positions increment by 1 for each non-skipped token. Stop words **do not** increment the position counter (they're simply dropped), which means there's no position gap where a stop word was. This matches the Java "SkippingStopFilter" behaviour, which differs from Lucene's default `StopFilter` that increments position even for removed tokens.

All tokens from a single raw input (stemmed+original for WORD, all sub-spans for COMPLEX) share the **same position**.

---

## Compound query tokenizer

**Source:** `tokenizer/compound/query.rs`
**Java equivalent:** `PatentSafeQueryAnalyser`
**Registered as:** `type: :compound, mode: :query`

### Pipeline overview

```
Input text
  │
  ├─ 1. Whitespace split
  ├─ 2. ASCII fold
  ├─ 3. Skip single-char punctuation tokens
  ├─ 4. Strip leading/trailing punctuation (preserving *, ?, ")
  ├─ 5. Lowercase
  ├─ 6. Stop word removal
  └─ 7. Stem + dual-emit (stemmed + original at same position)
```

### Key differences from the index tokenizer

| Aspect | Index tokenizer | Query tokenizer |
|--------|----------------|-----------------|
| **WORD/COMPLEX classification** | Yes | No |
| **N-gram expansion** | Yes (for COMPLEX) | No |
| **Punctuation stripping** | Configurable char sets | Strips everything except `*`, `?`, `"` |
| **Wildcard support** | No | Yes (`print*`, `print?` preserved) |
| **Quote preservation** | No | Yes (`"` kept for phrase queries) |
| **Stemming** | WORD tokens only | All non-stop tokens |

The query tokenizer is intentionally simpler because search queries don't need n-gram expansion — the **index** already contains all the sub-spans. A query for `"016"` will match the indexed sub-span `"016"` from `"E21634-016"` without any special query-side processing.

### Wildcard and phrase support

The query tokenizer preserves `*`, `?`, and `"` characters through the pipeline. This allows:

- `print*` — prefix wildcard search
- `print?` — single-character wildcard search
- `"exact phrase"` — phrase query (quotes are kept so the search layer can detect and build `PhraseQuery`)

### Example walkthrough

Input: `"~0.4 mg/mL in 25:75 methanol:water; prepared E21634-016"`

| Raw token | After pipeline | Notes |
|-----------|---------------|-------|
| `~0.4` | `0.4` | Leading `~` stripped |
| `mg/mL` | `mg/ml` | Lowercased, not a stop word |
| `in` | *(dropped)* | Stop word |
| `25:75` | `25:75` | Kept as-is (no leading/trailing to strip) |
| `methanol:water;` | `methanol:wat` (stemmed) + `methanol:water` (original) | Trailing `;` stripped, then stemmed + dual-emitted |
| `prepared` | `prepar` (stemmed) + `prepared` (original) | Stemmed + dual-emitted |
| `E21634-016` | `e21634-016` | Lowercased, not a stop word, stem is unchanged |

---

## How index and query tokenizers work together

The design principle: **expand at index time, simplify at query time.**

At index time, a document containing `"Experiment E21634-016 results"` produces:

```
Position 1: "experi", "experiment"          (WORD, stemmed + original)
Position 2: "e21634-016", "e", "e21634",   (COMPLEX, full + sub-spans)
            "21634", "21634-", "21634-016",
            "-016", "016", ...
Position 3: "result", "results"             (WORD, stemmed + original)
```

At query time, a user searching for `"016"` produces:

```
Position 1: "016"                           (lowercased, no expansion needed)
```

The query's `"016"` matches the indexed sub-span `"016"` at position 2. A query for `"E21634-016"` matches the full indexed token. A query for `"experiment results"` matches via the stemmed forms.

---

## Stop words

**Source:** `tokenizer/default.rs` (`english_stop_words()`) and `tokenizer/compound/stop_words.rs`

The default English stop word list matches Lucene 3.6's `ENGLISH_STOP_WORDS_SET`:

> a, an, and, are, as, at, be, but, by, for, if, in, into, is, it, no, not, of, on, or, such, that, the, their, then, there, these, they, this, to, was, will, with

Custom stop word lists can be provided as an array of strings:

```ruby
index.register_tokenizer("custom",
  type: :compound, mode: :index,
  stop_words: ["a", "the", "is", "custom_stop"])
```

---

## Java source references

The Rust tokenizer code was ported from these Java classes:

| Rust module | Java class | Purpose |
|-------------|-----------|---------|
| `compound/mod.rs` | `FullIndexingAnalyser` | Index tokenizer pipeline |
| `compound/query.rs` | `PatentSafeQueryAnalyser` | Query tokenizer pipeline |
| `compound/classifier.rs` | `BlockTokenParsingFilter` | WORD/COMPLEX classification |
| `compound/expander.rs` | `ComplexTokenFilter` | N-gram sub-span expansion |
| `compound/stop_words.rs` | `SkippingStopFilter` | Stop word filtering (position-preserving) |
| `compound/query.rs` (`strip_query_punctuation`) | `PreOrPostPunctuationStripFilter` | Query punctuation stripping |
| `compound/query.rs` (`is_query_filtered_char`) | `SkippingPunctuationStopFilter` | Single-char punctuation skip |
| `default.rs` | *(standard Lucene analysers)* | Baseline pipeline components |

The Rust test suite (`compound/tests.rs`) includes tests ported directly from the Java test classes: `ComplexTokenFilterTest`, `ComplexTokenFilterJJPDExamplesTest`, `ComplexTokenFilterDNAStringTest`, `ComplexTokenFilterChemicalReactionTest`, `FullIndexingAnalyserTest`, `BlockTokenParserTest`, and `PatentSafeQueryAnalyserTest`.
