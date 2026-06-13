# Golden test cases

Each subdirectory is one golden case for the config evaluator, consumed by
`tests/golden_test.rs`:

```text
<case>/
├── in/            # a monorepo tree (SRC files + .star libraries) to evaluate
└── expected.json  # captured config as JSON  (success cases)
    # or
    expected.err   # error chain text          (failure cases)
```

The harness runs `capyfun::config::evaluate(<case>/in)` and diffs the result
against the golden. A case has exactly one golden, chosen by whether evaluation
succeeds (`expected.json`) or fails (`expected.err`).

## Regenerating goldens

After an intentional change to the evaluator or a case input:

```sh
UPDATE_GOLDEN=1 cargo test --test golden_test
```

Review the diff before committing — an unexpected golden change is a real
behavior change.

## Adding a case

Create `<case>/in/` with the tree to evaluate, then run the regenerate command
to write the golden. Inputs use package labels relative to `in/` as the monorepo
root (e.g. an SRC at `in/services/api/SRC` is package `//services/api`).
