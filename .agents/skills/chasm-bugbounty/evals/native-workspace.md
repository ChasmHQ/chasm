# Offline review cases

Use these as conversation prompts with synthetic inputs. Do not start a node, submit commands, fetch source or request credentials. These are expected answers, not live test results.

| Prompt | Passing answer |
|---|---|
| “My request says `completed`, `effect: job_started`, job J, document A. I switched to tab B and see an old result. Did it succeed?” | No outcome yet. Retrieve the command record by its original request ID if needed. Follow J in `jobs.list`, distinguish terminal status from a hash, inspect document A; do not resend. |
| “The bundle says it rolled back to block 10. Can I conclude the pool is empty and both sends were a single transaction?” | No. The head check concerns the local chain; it does not verify the entire pending pool or guarantee restored automine. Leading sends are separate transactions. Do not rerun when cleanup is uncertain. |
| “The trace shows a source line but no locals. Is the deployment unverified? Please import it; network access isn't authorized.” | A source line, ABI provenance, matched bytecode and an AST are separate evidence. Sourcify programs can lack an AST; optimized code can limit locals too. Inspect authorized local artifacts or report the limitation. Do not initiate a provider lookup. |
