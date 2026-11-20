---
name: module-splitting
description: Split oversized source and test files into focused modules without changing behaviour. Use when asked to refactor a large file, break a module into modules, reorganize a package layout, or make a codebase easier to navigate and maintain.
metadata:
  trigger: A file has grown too large to navigate, splitting a module or package, reorganizing a test suite
---

# Splitting a large module

The goal is a reader who opens the tree cold and knows which file to open. That is a
different goal from "fewer lines per file", and it changes what a good split looks like:
boundaries follow concerns a person would name, and every module says what is inside it.

Behaviour must not change. A split that "probably works" is worse than no split, because
the next person now trusts a layout that lies.

## 1. Survey before cutting

```bash
wc -l <src>/**/*.py | sort -rn | head -20
grep -n "^class \|^def \|^# ==\|^# --\|^[A-Z_]* = " <file>
```

Consider splitting above ~500 lines, but size is the trigger, not the reason. The reason is
that the file holds concerns that change for different reasons. A 700-line module doing one
job well is fine. A 300-line module doing three jobs is not.

Existing section-divider comments are usually the seams the author already saw. Use them.

## 2. Choose the shape

**Package vs sibling modules.** Make it a package (`foo/` replacing `foo.py`) when the
pieces are only meaningful together and outside callers should keep importing `foo`. The
`__init__.py` re-exports the public names, so no importer changes. Use sibling modules when
each piece stands alone.

**Prefer explicit collaborators over mixins.** Splitting a class into
`class Big(MixinA, MixinB)` scatters one object across files with invisible coupling: the
reader cannot tell where `self.thing` is defined, and type checkers lose the attributes
(one such split produced 40 new `attr-defined` errors). Reach instead for:

- **Free functions taking the object explicitly**, as in `stepping.should_stop(session, ...)`.
  The call site names both the module and what it operates on.
- **A collaborator object** the owner holds, as in `self.code = CodeIndex(...)`, called as
  `self.code.artifact_for(...)`. Best when the extracted part owns state (caches,
  configuration), since it takes the fields with it.
- **A thin delegating method** on the original when the old name is public API, so callers
  and tests do not move.

**Grouping registries.** When a dispatch table maps names to handlers spread across
modules, let each module own its own entries (`VERBS = {...}` beside the implementations)
and have the dispatcher assemble them. Adding a case is then one function plus one line in
one file.

## 3. Cut mechanically, verify structurally

Extract by script rather than by hand, since a 200-line function retyped is a 200-line diff
to review. But **verify the result structurally, not by eye**:

```python
# every definition that existed must still exist somewhere
import ast, pathlib
before = {n.name for n in ast.parse(old_src).body
          if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))}
after = set()  # union over the new files
assert not before - after
```

For a test file, slice by **AST node span, not line range**. Line ranges cut functions in
half when a section boundary sits mid-body.

## 4. The five failure modes

Each of these passes lint and import, and fails only at run time.

1. **Deferred relative imports.** `from .decode import ...` written inside a function body
   still resolves at import time because it never executes. Moving the module one level
   deeper breaks it silently. Scan for them:
   ```python
   # every `from .x import` must name a real sibling
   for node in ast.walk(tree):
       if isinstance(node, ast.ImportFrom) and node.level == 1:
           assert node.module.split(".")[0] in siblings
   ```
   Worth keeping as a permanent test in any package-heavy layout.

2. **Dropped decorators.** Slicing from the `def` line leaves `@property` / `@staticmethod`
   behind. The method then silently becomes something else, and a `@property` turns into a
   bound method that is always truthy. Start each slice at the first decorator.

3. **Shadowed names.** A rewrite like `compile_standard(` -> `solc.compile_standard(` breaks
   wherever a local variable or parameter is also called `solc`. Grep the new module for
   locals sharing a name with an import.

4. **Monkeypatch targets.** `monkeypatch.setattr(mod, "f", ...)` only affects callers that
   look `f` up on `mod` at call time. If the caller moves and now does `from .x import f`,
   the patch is silently ineffective. Either keep the call site as `x.f(...)` and patch
   `x`, or update the test to patch the real home. Patching an attribute *on a third-party
   module object* (`solcx.get_versions`) keeps working either way.

5. **Frontend and test call sites.** Renaming a private helper to a public one is right when
   other modules now call it, but grep the whole repo, not just the package. A call inside a
   broad `except Exception:` will swallow the `AttributeError` and degrade silently.

## 5. Re-point and document

- Update importers: other packages, both frontends, and the tests.
- Where a test reaches a private name, point it at the real new home rather than
  re-exporting the private name to keep the old import working.
- Give each package `__init__.py` a docstring that maps its own modules, one line each.
  This is the payoff for the reader:
  ```
  events.py    the messages the two threads exchange
  core.py      `DebugSession`: the threads, the frame stack, the per-opcode hook
  stepping.py  when a step stops, and when a watchpoint fires
  ```
- Give each new module a docstring saying what it owns and why it is separate. Move the
  relevant paragraphs out of the old module's docstring rather than writing new prose.
- Load the `code-comment-style` skill for comment and docstring wording.

## 6. Gate every step

```bash
<lint> --fix && <format> && <typecheck> && <test>
```

Run the **full** suite, not the subset you think is affected: the failure modes above
surface in unrelated places. Compare the typechecker's error count against a baseline
(`git stash` and re-run) so a split that adds noise is visible.

Commit one coherent split at a time, with a message saying what the old file held and what
each new module owns. Do not batch several splits into one commit. When something breaks a
week later, the bisect wants them separate.

## What not to do

- Do not shred a file to hit a line count. A 40-line module with one function used once is
  worse than leaving it where it was.
- Do not create a re-export shim for the old module name "for compatibility" inside a
  codebase you control. Update the importers.
- Do not change behaviour, rename public API, or fix unrelated bugs in the same commit as a
  move. If a real bug surfaces mid-split (a duplicated cache key, a shadowed name), note it
  and fix it in its own commit.
- Do not leave the old file behind until the parity check passes.
