# Adding a Phase

SuperSip v2.0 development follows a phased approach tracked in `.planning/`.

## Directory Structure

```
.planning/
├── PROJECT.md          # Project context and key decisions
├── REQUIREMENTS.md     # All requirements with IDs
├── ROADMAP.md          # Phase list with goals and success criteria
├── MILESTONES.md       # Milestone history
├── STATE.md            # Current progress
└── phases/
    ├── 01-api-shell-cheap-wrappers/
    │   ├── 01-CONTEXT.md      # Phase boundary and decisions
    │   ├── 01-01-PLAN.md      # Individual task plans
    │   ├── 01-02-PLAN.md
    │   └── 01-VERIFICATION.md # Completion report
    └── 02-trunk-groups-schema-core-crud/
        ├── 02-CONTEXT.md
        ├── 02-01-PLAN.md
        └── ...
```

## Phase Lifecycle

1. **Discuss** — Define boundary, decisions, files touched -> `NN-CONTEXT.md`
2. **Plan** — Break into tasks with must-haves and artifacts -> `NN-XX-PLAN.md`
3. **Execute** — Implement each plan, commit atomically
4. **Verify** — Check success criteria against code -> `NN-VERIFICATION.md`

## Adding a New Phase

1. Add the phase to `.planning/ROADMAP.md` with goal, dependencies, requirements, and success criteria
2. Create the phase directory: `.planning/phases/NN-<slug>/`
3. Write `NN-CONTEXT.md` defining the boundary
4. Create plans as `NN-XX-PLAN.md` files
5. Update `.planning/STATE.md` when starting execution

## Phase Numbering

- Integer phases (1, 2, 3, ...): Planned milestone work in sequence
- Decimal phases (2.1, 2.2, ...): Urgent insertions between existing phases (marked INSERTED)

## Reading a CONTEXT.md

- **Boundary** — what is in scope and what is not
- **Decisions** — locked technical choices (e.g., table schema, adapter pattern)
- **Files touched** — new and modified files
- **Validation** — how to verify the phase is complete

## Reading a PLAN.md

- **Objective** — one-sentence goal
- **Must-haves** — non-negotiable requirements
- **Artifacts** — files to create/modify with descriptions
- **Interfaces** — API contracts, types, function signatures

## Success Criteria

Every phase in `ROADMAP.md` lists explicit success criteria — statements that must be TRUE before the phase is considered complete. The `NN-VERIFICATION.md` file records the result of checking each criterion against the actual codebase.
