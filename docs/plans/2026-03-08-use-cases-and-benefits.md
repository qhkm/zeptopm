# ZeptoPM Orchestration — Use Cases and Benefit Analysis

**Date:** 2026-03-08
**Companion to:** `2026-03-08-orchestration-design.md`

---

## 1. Current Capabilities (What Already Works)

Before analyzing what orchestration adds, it's worth being precise about what zeptoPM can do today:

### Use Case A: Standalone chat agents

```
User → zeptopm chat researcher "What is quantum computing?"
     → researcher agent responds
```

**Works today.** Each agent is a persistent, long-lived process with session memory. Good for:
- Personal AI assistants
- Customer support bots
- Knowledge base Q&A
- Any single-agent, conversational workload

### Use Case B: Linear pipeline

```
zeptopm pipeline "researcher,writer" "Write about AI trends in Malaysia"
  Step 1: researcher produces findings
  Step 2: writer receives findings, produces article
```

**Works today.** Output of agent N feeds into agent N+1 as a chat message. Good for:
- Research → write workflows
- Translate → review chains
- Extract → summarize pipelines

### Use Case C: Manager delegation

```
zeptopm orchestrate manager "Build a competitive analysis report"
  Round 1: manager decides to delegate → @delegate(researcher): ...
  Round 2: researcher results fed back → manager synthesizes
```

**Works today.** Manager LLM decides who to delegate to via @delegate markers. Good for:
- Simple coordination tasks
- When the manager can decompose on-the-fly
- Low-stakes tasks where imprecise delegation is acceptable

### Limitations of current capabilities

| Limitation | Impact |
|-----------|--------|
| Pipeline is strictly linear | Can't run researcher + analyst in parallel |
| @delegate is LLM-parsed text | Fragile — depends on LLM formatting correctly |
| No structured handoff | Results pass as chat strings, not structured data |
| No dependency graph | Can't express "C depends on A and B" |
| No per-step retry | One failure kills the whole pipeline |
| No progress visibility | Can't see which step is running or stuck |
| No artifact trail | No audit of intermediate outputs |

---

## 2. Use Cases That Require Orchestration

### UC-1: Deep Research Report

**Scenario:** "Produce a comprehensive market analysis of AI startups in Southeast Asia"

**Why current system fails:**
- Too complex for a single agent (needs web research, data analysis, writing)
- Pipeline is linear but research tasks are independent (can parallelize)
- No structured handoff of research findings to analyst

**With orchestration:**
```
Run: "Market analysis of AI startups in SEA"
  |
  Planner job → produces ExecutionPlan:
    |
    +-- researcher_1: "Research AI startups in Malaysia"     (parallel)
    +-- researcher_2: "Research AI startups in Indonesia"    (parallel)
    +-- researcher_3: "Research AI startups in Singapore"    (parallel)
    |
    +-- analyst: "Synthesize findings into market gaps"
    |     depends_on: [researcher_1, researcher_2, researcher_3]
    |     input_artifacts: [findings_my.json, findings_id.json, findings_sg.json]
    |
    +-- writer: "Write final report"
    |     depends_on: [analyst]
    |     input_artifacts: [analysis.json]
    |
    +-- reviewer: "Review report for accuracy"
          depends_on: [writer]
          input_artifacts: [report.md]
```

**Benefit:** 3 research tasks run in parallel (3x faster). Structured JSON artifacts instead of chat strings. Reviewer can request revisions. Each step is independently retryable.

**Time comparison:**
- Current (linear pipeline): ~5 min (sequential research + analysis + writing)
- With orchestration: ~2.5 min (parallel research, then sequential analysis + writing)

---

### UC-2: Code Generation with Review

**Scenario:** "Implement a REST API for user authentication with JWT"

**Why current system fails:**
- Coder produces code, but nobody reviews it
- No structured way to feed review feedback back to coder
- Can't separate planning from implementation

**With orchestration:**
```
Run: "Implement JWT auth API"
  |
  Planner → ExecutionPlan:
    |
    +-- coder: "Implement /register, /login, /refresh endpoints"
    |
    +-- reviewer: "Review code for security, correctness, style"
    |     depends_on: [coder]
    |     input_artifacts: [code_patch.diff]
    |
    [If reviewer says "revise":]
    +-- coder (retry): "Fix issues: {reviewer feedback}"
    |     input_artifacts: [review_report.json]
    |
    +-- reviewer (retry): "Re-review after fixes"
```

**Benefit:** Automatic review loop. Coder gets structured feedback, not vague chat. Review decision is machine-parseable (approved/revise/rejected). Each iteration is tracked as a separate job attempt.

---

### UC-3: Content Production Pipeline

**Scenario:** "Create a blog post series about Rust programming (5 posts)"

**Why current system fails:**
- 5 posts = 5 independent writing tasks, but pipeline is linear
- No way to have an editor review all 5 posts for consistency
- No artifact trail of drafts vs finals

**With orchestration:**
```
Run: "Rust blog series"
  |
  Planner → ExecutionPlan:
    |
    +-- writer_1: "Post 1: Ownership and Borrowing"     (parallel)
    +-- writer_2: "Post 2: Error Handling with Result"   (parallel)
    +-- writer_3: "Post 3: Async/Await in Practice"      (parallel)
    +-- writer_4: "Post 4: Traits and Generics"          (parallel)
    +-- writer_5: "Post 5: Building a CLI App"            (parallel)
    |
    +-- editor: "Review all 5 posts for consistency, tone, accuracy"
    |     depends_on: [writer_1..5]
    |     input_artifacts: [post_1.md, post_2.md, post_3.md, post_4.md, post_5.md]
    |
    +-- finalizer: "Apply editor feedback, produce final versions"
          depends_on: [editor]
```

**Benefit:** 5 posts written in parallel instead of sequentially. Editor sees all posts together for consistency review. Clear artifact trail: draft → review notes → final.

**Time comparison:**
- Current (sequential): ~25 min (5 posts x ~5 min each)
- With orchestration: ~7 min (parallel writing + sequential review)

---

### UC-4: Data Processing Pipeline

**Scenario:** "Analyze our customer support tickets from last month, find patterns, recommend improvements"

**Why current system fails:**
- Multi-phase: extract → analyze → synthesize → recommend
- Analysis phase has natural parallelism (analyze by category)
- Need structured JSON between phases, not chat strings

**With orchestration:**
```
Run: "Support ticket analysis"
  |
  Planner → ExecutionPlan:
    |
    +-- extractor: "Parse raw tickets into structured format"
    |     output: tickets.json
    |
    +-- analyst_bugs: "Analyze bug-related tickets"          (parallel)
    +-- analyst_features: "Analyze feature request tickets"  (parallel)
    +-- analyst_ux: "Analyze UX complaint tickets"           (parallel)
    |     all depend_on: [extractor]
    |     all input: tickets.json
    |
    +-- synthesizer: "Combine analyses into unified findings"
    |     depends_on: [analyst_bugs, analyst_features, analyst_ux]
    |
    +-- recommender: "Produce actionable recommendations"
          depends_on: [synthesizer]
```

**Benefit:** Fan-out / fan-in pattern. Three analysts run in parallel on different ticket categories. Structured JSON flows between phases. Each analyst is independently retryable.

---

### UC-5: Multi-Source Fact Checking

**Scenario:** "Fact-check this article against multiple sources"

**Why current system fails:**
- Need to check multiple claims independently (parallel)
- Need to aggregate results with confidence scores
- Current pipeline can't fan out and fan back in

**With orchestration:**
```
Run: "Fact-check article"
  |
  Planner → ExecutionPlan:
    |
    +-- splitter: "Extract individual claims from article"
    |     output: claims.json (list of claims)
    |
    +-- checker_1: "Verify claim 1 against sources"   (parallel)
    +-- checker_2: "Verify claim 2 against sources"   (parallel)
    +-- checker_3: "Verify claim 3 against sources"   (parallel)
    |     all depend_on: [splitter]
    |
    +-- aggregator: "Compile fact-check report with confidence scores"
          depends_on: [checker_1, checker_2, checker_3]
          output: fact_check_report.json
```

**Benefit:** N claims checked in parallel. Each check is independent and retryable. Final report has structured confidence scores, not just a chat summary.

---

## 3. Benefit Analysis

### Quantitative benefits

| Metric | Current (no orchestration) | With orchestration | Improvement |
|--------|---------------------------|-------------------|-------------|
| **Parallel execution** | 1 agent at a time per pipeline | N agents concurrently | Up to Nx speedup |
| **Retry granularity** | Restart entire pipeline | Retry individual job | Saves N-1 jobs' worth of API cost |
| **Data fidelity** | Chat string (lossy) | JSON artifact (structured) | Eliminates parsing errors |
| **Failure blast radius** | Whole pipeline fails | Single job fails | Other jobs unaffected |
| **Progress visibility** | None | Per-job status + phase | Know exactly what's running |
| **Audit trail** | None | Artifacts on disk | Full intermediate outputs preserved |

### Cost analysis

For a typical 5-step research workflow:

| Scenario | API calls | Elapsed time | Cost (GPT-4o-mini) |
|----------|-----------|-------------|---------------------|
| Sequential pipeline | 5 serial | ~5 min | $0.05 |
| With orchestration (3 parallel + 2 serial) | 5 total (same) | ~3 min | $0.05 |
| Sequential fails at step 4, restart all | 4 + 5 = 9 | ~9 min | $0.09 |
| Orchestration fails at step 4, retry only step 4 | 5 + 1 = 6 | ~4 min | $0.06 |

**Key insight:** Orchestration doesn't increase API cost — same number of LLM calls. It reduces elapsed time (parallelism) and waste on failure (per-job retry).

### Qualitative benefits

**For solo developers (primary audience):**
- Run complex multi-agent tasks without manual coordination
- "Submit and forget" — check back when the run completes
- Artifact trail gives confidence in results (not a black box)
- Per-job retry means less babysitting

**For teams:**
- Standardized workflows via planner profiles
- Reproducible runs (same plan → same job graph)
- Audit trail for compliance/review

**For the zeptoPM product:**
- Key differentiator vs simple agent wrappers (LangChain, CrewAI)
- Moves from "chat with agents" to "submit complex tasks"
- Natural upgrade path: standalone agents → pipelines → orchestrated runs

---

## 4. Use Cases That Do NOT Need Orchestration

Not everything benefits from the orchestration layer. These are fine with current zeptoPM:

| Use Case | Why orchestration is overkill |
|----------|------------------------------|
| Single chatbot | One agent, ongoing conversation — just use `zeptopm chat` |
| Simple Q&A | No multi-step workflow — just use `zeptopm chat` |
| Two-agent pipeline | Already works with `zeptopm pipeline` |
| Manager + 2 helpers | Already works with `zeptopm orchestrate` |
| Long-running monitoring agent | Standalone agent with session persistence — already works |
| API gateway to single agent | Gateway with auth + rate limiting — already works |

**Rule of thumb:** If the task has <3 steps and no parallelism, existing zeptoPM is sufficient. Orchestration adds value when there are 3+ steps, parallelizable subtasks, or structured handoff requirements.

---

## 5. Competitive Positioning

### Where zeptoPM sits today

| Tool | What it does | Our advantage |
|------|-------------|---------------|
| **PM2** | Node.js process manager | We're purpose-built for AI agents, not web servers |
| **LangChain** | Python LLM framework | We're a runtime, not a library — process isolation, persistence |
| **CrewAI** | Python multi-agent framework | We're language-agnostic, process-isolated, config-driven |
| **AutoGen** | Microsoft multi-agent | We're simpler, lower overhead, single binary |
| **Temporal** | Durable execution engine | We're AI-native, not general-purpose workflow |

### Where zeptoPM sits after orchestration

| Capability | LangChain | CrewAI | AutoGen | Temporal | **zeptoPM** |
|-----------|-----------|--------|---------|----------|------------|
| Process isolation | No | No | No | Yes | **Yes** |
| Session persistence | Plugin | No | Plugin | Yes | **Yes** |
| Parallel execution | Manual | Limited | Yes | Yes | **Yes** |
| Dependency graph | No | No | Limited | Yes | **Yes** |
| Artifact handoff | No | No | No | Yes | **Yes** |
| Per-job retry | No | No | No | Yes | **Yes** |
| Config-driven | No | YAML | No | No | **TOML** |
| Single binary | No | No | No | No | **Yes** |
| Memory footprint | ~100MB+ | ~200MB+ | ~150MB+ | ~500MB+ | **~4MB/agent** |

**The positioning:** zeptoPM is the Temporal of AI agents — durable execution with supervision — but in a single 11 MB binary with 4 MB per agent instead of a JVM cluster.

---

## 6. Priority Matrix

Based on use case frequency and benefit magnitude:

| Use Case Pattern | Frequency | Orchestration Benefit | Priority |
|-----------------|-----------|----------------------|----------|
| Fan-out research (UC-1, UC-5) | High | High (parallelism) | **P0** |
| Code + review loop (UC-2) | High | High (quality) | **P0** |
| Parallel content production (UC-3) | Medium | High (speed) | **P1** |
| Multi-phase data pipeline (UC-4) | Medium | Medium (structure) | **P1** |
| Single agent chat | Very high | None (already works) | N/A |
| Simple 2-agent pipeline | High | None (already works) | N/A |

**Recommendation:** Implement the orchestration layer. The top use cases (fan-out research, code review loops) are high-frequency and high-benefit. The implementation cost is moderate (layering on existing infra, not rebuilding).

---

## 7. Success Criteria

How we'll know the orchestration layer is working:

1. **Fan-out demo:** Submit "research AI startups in 3 countries" → 3 researchers run in parallel → analyst synthesizes → report produced. Total time < 1.5x single-researcher time.

2. **Review loop demo:** Submit "implement a function" → coder produces code → reviewer reviews → revision cycle completes automatically.

3. **Failure recovery demo:** Kill a worker mid-run → supervisor retries just that job → run completes successfully.

4. **Artifact inspection:** After a run, `ls ~/.zeptopm/artifacts/{run_id}/` shows structured JSON files for each job's output.

5. **CLI usability:** `zeptopm run submit "task"` → `zeptopm run status {id}` shows job-by-job progress → `zeptopm run result {id}` shows final output.
