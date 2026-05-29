# บทที่ 25 — Workflows

Workflows คือ **orchestration tier ที่สี่** ของ thClaws — Claude
เขียนสคริปต์ JavaScript ที่กระจายงานไปยัง subagent หลายตัว แล้ว JS
engine ในตัวสคริปต์รันแบบ deterministic บนเครื่องของคุณ ต่างจาก
subagent (บทที่ 15), `/agent` side-channel, หรือ Agent Teams (บทที่
17) ตรงที่ตัวสั่งการคือ **code** ไม่ใช่ model — ซึ่งหมายความว่ารัน
workflow เดิมซ้ำจะได้รูปทรงงานเหมือนเดิมทุกครั้ง และงานยาว ๆ จะ
เหลือ checkpoint ไว้บนดิสก์

Workflows เป็น **Tier 1** ใน v0.23 — fan-out ใช้ได้แล้ว ส่วน schema
validation กับ resume เป็นเรื่องของ Tier 2 (ดู "สิ่งที่ยังไม่มีใน
Tier 1" ด้านล่าง)

## ควรใช้ workflows เมื่อไร

ใช้ workflows กับ **งาน bulk ที่อิสระจากกันและต้องการความแน่นอน**:

- "rewrite test file 800 ไฟล์ให้ใช้ fixture ใหม่"
- "แปลทุก `.md` ใต้ `kms/bug/` เป็นภาษาไทย"
- "audit `Cargo.toml` ของแต่ละ crate แล้ว flag deps ที่ deprecated"

ใช้ `Task` tool (บทที่ 15) กับ **side-quest ที่ model ตัดสินใจสร้าง
ขึ้นมาเอง** กลางเทิร์น — นั่นคือสิ่งที่ subagent ทำต่อไป

ใช้ `/agent` (บทที่ 15) เมื่อ **คุณ** รู้ชัดว่าจะให้ specialist ทำ
อะไร และอยากให้ทำงานคู่ขนานกับ session หลัก

ใช้ Agent Teams (บทที่ 17) เมื่อ teammate ต้อง **ร่วมมือกัน** —
แลก message ถกเถียงสมมติฐาน ประสานงานบน task list ร่วมกัน
Workflows เป็น stateless fan-out ส่วน team เป็น stateful collaboration

## เริ่มใช้

```text
/workflow run summarize each .rs file under src/ in one line
```

ลำดับเหตุการณ์:

1. **Author phase** Claude เขียนสคริปต์ JavaScript ที่ใช้ API
   `thclaws.*` (รายละเอียด API อยู่ใน system prompt ของ model
   อยู่แล้ว ดังนั้นสคริปต์ที่ได้กลับมารู้ว่ามีอะไรให้ใช้บ้าง)
2. **Review** สคริปต์ถูก print พร้อมเลขบรรทัด แล้วถาม:
   ```text
   [a]pprove · [c]ancel · [r]e-author:
   ```
   - `a` — รันตามนี้
   - `c` — ยกเลิก
   - `r` — ใส่ note บรรทัดเดียวบอกว่าให้แก้อะไร ("ใช้ read tool ไม่
     ใช่ bash cat") แล้ว Claude เขียนสคริปต์ใหม่ตาม feedback วน
     จนกว่าจะกด `a` หรือ `c`
3. **Execute** แสดง workflow id (`wf-…`) จากนั้นทุก subagent call
   จะมีบรรทัด progress:
   ```text
   ✓ w0  List every .rs file under src/, recursively. Return o…   2s
   ✓ w1  Read crates/core/src/agent.rs and write ONE sentence …   3s
   ✓ w2  Read crates/core/src/repl.rs and write ONE sentence d…   4s
   …
   workflow done — 47 workers, total 1m 12s
   crates/core/src/agent.rs — the streaming agent loop
   crates/core/src/repl.rs — REPL command parser + rustyline I/O
   …
   ```

ถ้า worker error จะเห็น `✗ wN  …` และสคริปต์มักจะ catch แล้วทำงาน
ต่อ (แล้วแต่ Claude เขียน)

## API `thclaws.*`

สคริปต์ของคุณได้ global ตัวเดียว — `thclaws` — มี field ต่อไปนี้:

```js
thclaws.subagent({
  prompt: string,           // จำเป็น — งานของ worker
  // schema?, budget?, retry?, model? — Tier 2; ignored ใน Tier 1
}) → string                 // text สุดท้ายที่ worker ตอบ
```

แค่นั้นใน Tier 1 Worker จะ inherit provider, model, system prompt,
tool registry, memory, KMS, และ permission mode จาก session แม่ —
ดังนั้น worker ใช้ `Bash`, `Read`, `Edit`, search KMS, MCP server
ได้หมด การ recurse ของ subagent (worker เรียก Task เอง) ถูกจำกัด
ด้วย `DEFAULT_MAX_DEPTH = 3` เหมือนกับ subagent ปกติ

**`thclaws.subagent` เป็น synchronous ใน Tier 1** — คืน text ของ
worker กลับมาตรง ๆ ไม่ใช่ Promise ห้ามเขียน `await
thclaws.subagent(...)` เด็ดขาด เพราะ sandbox รัน Boa ใน Script mode
ที่ไม่รองรับ top-level `await` (เป็น SyntaxError ทันทีก่อนสคริปต์
จะรัน) async + `Promise.all` parallelism จริงจะมาใน Tier 2 พร้อม
Module-mode execution

### เขียนอะไรในสคริปต์ได้บ้าง

JS control flow มาตรฐาน: `for`, `while`, `if`/`else`, `try`/`catch`,
destructuring, template literal, array/string method, regex, JSON
parsing

### เขียนอะไรไม่ได้

- `await`, `async` function, `Promise.*` (Tier 2)
- `eval`, `Function` (ถูกปลดจาก sandbox)
- `fetch`, `require`, `process`, DOM, `console.log`

ของที่จะ I/O ต้องผ่าน subagent

### ตัวอย่างสั้น ๆ

```js
// Workflow: list .rs files, summarise each
const list = thclaws.subagent({
  prompt: "List every .rs file under src/, recursively. Paths only."
});
const paths = list.split("\n").map(s => s.trim()).filter(Boolean);

const summaries = paths.map(p => thclaws.subagent({
  prompt: `Read ${p} and write ONE sentence describing what it does.`
}));

paths.map((p, i) => `${p} — ${summaries[i]}`).join("\n");
```

**expression สุดท้ายของสคริปต์** คือสิ่งที่กลายเป็น output ของ
assistant turn — ในที่นี้คือ list ที่ join แล้ว

## State บนดิสก์

ทุกครั้งที่รัน workflow จะเขียน JSONL log ลง:

```text
.thclaws/workflows/wf-<id>/state.jsonl
```

หนึ่ง event ต่อบรรทัด flush หลังเขียนทุกครั้งเพื่อให้ Ctrl-C ไม่
ทิ้งไฟล์ค้างกลางคัน รูปแบบ event:

```jsonl
{"ts":"…","kind":"start","id":"wf-…","prompt":"…","script_sha":"…","script_chars":234}
{"ts":"…","kind":"worker_start","id":"wf-…","worker":"w0","prompt":"…"}
{"ts":"…","kind":"worker_done","id":"wf-…","worker":"w0","output":"…"}
{"ts":"…","kind":"worker_error","id":"wf-…","worker":"w1","error":"…"}
{"ts":"…","kind":"done","id":"wf-…","result":"…"}
```

`cat`, `grep`, `jq` ไฟล์ได้ตลอดเวลา — เป็น JSONL ธรรมดา ไม่มี
ฟอร์แมตปิด Tier 2 จะเพิ่ม `/workflow list`, `/workflow inspect
<id>`, และ `/workflow rm <id>` จะได้ไม่ต้องเข้า directory เอง

ถ้า `.thclaws/` เขียนไม่ได้ (read-only volume, permission)
workflow ยังรันแต่จะ print:
```text
/workflow run: state.jsonl unavailable — proceeding without checkpoint
```
audit trail หายไป แต่ run ไม่หาย

## Headless mode

`thclaws -p "/workflow run <goal>"` **ถูกปฏิเสธ** Author phase
สร้างสคริปต์ที่ต้องให้คุณรีวิวก่อนรัน `-p` ไม่มี surface ให้รีวิว
และการ default-approve สคริปต์ที่ไม่ได้ดูเป็นเรื่องอันตราย

สคริปต์ที่เขียนไว้ล่วงหน้ารัน headless ได้ผ่าน `thclaws --workflow
<file.js>` — เป็น Tier 2 (ต้องมี file-input plumbing + `--resume`
ก่อน) ระหว่างนี้ workflow ต้องอยู่ใน REPL แบบ interactive

## สิ่งที่ยังไม่มีใน Tier 1

นี่คือช่องว่างที่รู้อยู่ ไม่ใช่ bug — จะมาใน Tier 2 / 3 ตาม
[dev-plan/32](../dev-plan/32-dynamic-workflows.md) (workspace-only):

- **Subagent call เป็น synchronous ไม่มี `await` / `Promise`** Boa
  รัน script ใน Script mode ที่ top-level `await` เป็น SyntaxError
  เราจึง expose `thclaws.subagent(...)` เป็นฟังก์ชัน synchronous
  ที่คืน text กลับมาตรง ๆ subagent call จะ fan-out แบบ sequential
  ตามลำดับใน source — wall clock เท่ากับ "ผลรวม" ของ latency ไม่
  ใช่ "ค่ามากที่สุด" Tier 2 จะใส่ Module mode + tokio-integrated
  job executor ให้ `await`, `async`, `Promise.all` กลับมาพร้อม
  parallelism จริง ระหว่างนี้เขียน script ถือเป็น serial
- **ยังไม่มี schema validation** ตัวเลือก `schema:` ถูกรับแต่ ignored
  worker ตอบ text อิสระ Tier 2 จะใส่ `jsonschema` validation + auto
  retry เมื่อ shape ไม่ตรง
- **ยังไม่มี `--resume`** state.jsonl log ถูกเขียนแต่ยังไม่อ่านกลับ
  ถ้าระบบล่มระหว่างรัน 200-worker workflow ต้องเริ่มใหม่ Tier 2
  จะใช้ log-replay resume พร้อม call-site matching เพื่อไม่ spawn
  worker ที่เสร็จแล้วซ้ำ
- **ยังไม่มี budget cap** Per-worker `budget: { tokens, time }`
  ignored Tier 2 จะ enforce
- **ยังไม่มี verification phase** `thclaws.verify({...})` ยังไม่มี
  — Tier 3
- **ยังไม่มี GUI worker grid** จาก chat tab `/workflow run` ถูก
  ปฏิเสธพร้อมข้อความ 1 บรรทัด UX ของรีวิวแบบ interactive ไม่
  เหมาะกับ chat bubble และ grid ของ worker progress แบบ real time
  เป็นงาน frontend ของ Tier 3

## เรื่อง cost

ทุก `thclaws.subagent` call เป็น model turn แยก — ปกติไม่กี่วินาที
และไม่กี่ร้อยถึงไม่กี่พัน token Workflow 200 worker อาจกิน $5–$20
ของ API token ได้ง่าย ๆ ขึ้นกับ model มี 2 guard ใช้งานจริง:

- **จำกัด fan-out ก่อนเขียนสคริปต์** ถ้าเป้าหมายไม่มีขอบเขต ("ทุก
  ไฟล์") ให้ discovery subagent คืน list ก่อนจะได้เห็น cardinality
  ก่อน approve สคริปต์
- **ดูบรรทัดสรุปปิดท้าย** `workflow done — N workers, total Xm Ys`
  บอกเวลาที่ใช้บน wall clock Tier 2 จะเพิ่มสรุป token + เงินใน
  บรรทัดนั้น

## ตารางอ้างอิงเร็ว

| | Subagent (`Task`) | `/agent` | Agent Teams | Workflow |
|---|---|---|---|---|
| ตัวสั่งการ | Model | คุณ (one-shot) | Team-lead model | Code |
| จำนวน worker | 1 (blocking) | 1 (concurrent) | 3–5 collaborator | สิบถึงร้อย |
| worker คุยกันเอง | ไม่ได้ | ไม่ได้ | ได้ (mailbox) | ไม่ได้ (stateless) |
| Determinism | Model-driven | Model-driven | Model-driven | Deterministic execution |
| Resume ได้ | ไม่ | ไม่ | จำกัด | บันทึก log (Tier 2 อ่านกลับ) |
| เหมาะกับ | Side-quest กลางเทิร์น | Specialist ทำงานคู่ขนาน | ถกเถียง / ร่วมมือ | Bulk fan-out |

## Troubleshooting

**"workflow: state.jsonl unavailable — proceeding without checkpoint"**
— `.thclaws/workflows/` สร้างหรือเขียนไม่ได้ ตรวจ permission ของ
`.thclaws/` ใน project root

**Script error: `ReferenceError: thclaws is not defined`** — คุณ
น่าจะรันสคริปต์นอก `/workflow run` global `thclaws.*` มีอยู่เฉพาะ
ใน workflow sandbox

**Workflow ค้างหลังบรรทัด `⠋ wN  …`** — worker ตัวนั้นกำลังใช้เวลา
นาน Tier 1 ยังไม่มี timeout ต่อ subagent call กด Ctrl-C จะหยุดทั้ง run

**Re-author loop ได้สคริปต์เดิมซ้ำ ๆ** — Claude อาจมอง revision
note ของคุณข้าม ลองยกเลิกแล้วรันใหม่โดยเขียน goal ให้ชัดขึ้น แทน
การพึ่ง `r`-loop
