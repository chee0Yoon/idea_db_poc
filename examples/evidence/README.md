# Recorded atomization experiment

Synthetic Korean inputs are in `tests/evidence_corpus.py:ATOMIZATION_CASES`.
Main sent only source text and the JSON contract, without gold guards, through
existing local ACP clients to Grok 4.6 and Fable 5.1 on 2026-09-07. Inference used
those providers; this was not on-device generative inference. No production data
or secrets were used. Replaying these fixtures makes no paid model calls.

`model-responses-reviewed.json` is the explicit client review artifact used for
MCP round-trip testing, not an unedited first-attempt benchmark:

- Grok's first response was valid JSON: 12 cases, 26 units. Main retained it.
- Fable's first ACP result contained two concatenated JSON objects. It also added
  an unsupported claim that draft v1 had never been adopted. Main rejected that
  first response and requested a separately logged correction.
- The correction removed the unsupported claim but was again delivered as two
  JSON objects differing in the default-rule wording in `exception`. Main read
  both and explicitly selected the second object (12 cases, 30 units). No general
  automatic last-object parser is used. Whether duplication originates in the
  model or adapter/hook was not isolated.
- Pronouns and exceptions are interpreted with each source paragraph retained as
  the parent Core. These units are not claimed to be context-free propositions.
  Units marked pending remain Candidates and are excluded from composition.

First attempts, correction, prompts, provider/model IDs, parse failures and
coverage measurements are retained locally under
`test-results/evidence-reviews/` and `test-results/atomization-scoring-r1/`.
Coverage counts are source-citation diagnostics, not entailment or semantic
atomicity scores. These 12 synthetic cases do not establish provider-wide quality.
