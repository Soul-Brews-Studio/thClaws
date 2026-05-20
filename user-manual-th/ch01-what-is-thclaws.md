# บทที่ 1 — thClaws คืออะไร?

![logo](../user-manual-img/logo/thClaws-logo-line-art-banner.png)

thClaws คือ **AI Agent Platform ที่เขียนด้วย Rust แบบ native** รันบน
เครื่องของคุณเอง สำหรับสร้าง AI Agent มาช่วยคุณทำงานหลากหลาย เช่น
เขียนโปรแกรม ทำงานอัตโนมัติ ตรวจสอบและจัดระเบียบเอกสาร จัดการ
Knowledge Base หรือสร้างทีม AI Agent ทำงานร่วมกัน — ทั้งหมดรวมอยู่ใน
binary เดียว แค่บอกเป็นภาษาธรรมชาติว่าต้องการอะไร แล้ว agent จะอ่านไฟล์
รันคำสั่ง ใช้ tool และพูดคุยโต้ตอบกับคุณระหว่างทำงาน

หก surface รวมอยู่ใน binary เดียว ใช้ `Agent` loop, `Session` และ tool
registry ชุดเดียวกัน — ห้า surface แรกสำหรับ "คน" คนหนึ่ง (รวมถึงคุย
ผ่าน LINE บนมือถือ), surface ที่หกให้ "ซอฟต์แวร์อื่น" เรียกใช้ thClaws
ไปทำงาน:

- **Desktop GUI** (`thclaws` โดยไม่ใส่ flag) — หน้าต่าง native ประกอบด้วย
  แท็บ Terminal ที่รัน REPL ตัวเดียวกับโหมด `--cli`, แท็บ Chat แบบ
  streaming, Files browser และแท็บ Team (ตัวเลือกเสริม)
- **CLI REPL** (`thclaws --cli`) — prompt โต้ตอบใน terminal เหมาะกับการใช้
  ผ่าน SSH, เซิร์ฟเวอร์ headless หรือเมื่อไม่ต้องการ overhead ของ GUI
- **โหมดไม่โต้ตอบ** (`thclaws -p "prompt"` รูปแบบเต็มคือ `--print`)
  — รันแค่หนึ่ง turn แล้วออก สะดวกสำหรับสคริปต์ CI pipeline หรือ
  one-liner ใน shell ใส่ `-v` / `--verbose` เพื่อให้แสดง token usage
  ของ turn บน stderr โดยไม่รบกวน stdout
- **Webapp** (`thclaws --serve --port 7878` + เปิด browser) — engine
  ตัวเดียวกันผ่าน WebSocket/HTTP เปิดจาก laptop คุณเอง เข้าถึงระยะไกล
  ผ่าน SSH tunnel ได้ — "thClaws ทุกที่" โดยไม่ต้องเปิด port
- **LINE Chat** (`thclaws --line` หรือ GUI Line Connect modal) —
  คุยกับ agent ผ่าน LINE OA ของคุณเอง ทำงานผ่าน relay tunnel ที่
  `line.thclaws.ai` ซึ่งเชื่อมระหว่าง LINE platform กับ thClaws ที่รัน
  บนเครื่องคุณ — agent อยู่ในเครื่องคุณ แต่เรียกใช้ได้จากที่ไหนก็ได้
  ผ่านมือถือ (ดู [บทที่ 21](ch21-line-and-browser-chat.md))
- **AI Agent (API Server)** (`thclaws --serve` + HTTP API) — ให้
  *ซอฟต์แวร์อื่น* (orchestrator, external client, scheduler) เรียกใช้
  thClaws เป็น agent ผ่าน HTTP API เดียวกัน — รายละเอียดอยู่ในบทถัด ๆ ไป

## สิ่งที่ทำให้ thClaws แตกต่าง

- **Self-improving AI Agent (auto-learn)** — เปิด `autoLearn: true` ใน
  settings แล้ว agent จะเรียนรู้จากตัวเองอัตโนมัติ ทุก session ที่จบลง
  จะถูกบันทึกเป็น KMS page ใน `self_learn` (แยกจาก KMS ที่คุณ curate
  เอง) และตามรอบ (default ทุก 6 ชั่วโมง) จะ reconcile contradictions
  ในนั้น สร้างจากปริมิทีฟที่มีอยู่แล้ว (`/kms ingest`, `/kms reconcile`)
  — ไม่มี prompt agent ใหม่ แค่เปิด/ปิดด้วย flag เดียว ถ้าอยากเริ่มใหม่
  ลบโฟลเดอร์ `self_learn/` ทิ้งได้เลย ([บทที่ 9 §Self-improving AI Agent](ch09-knowledge-bases-kms.md#self-improving-ai-agent-auto-learn))
- **3 ระดับของ agent orchestration** —
  **`Task` tool** (model ตัดสินใจ block parent's turn),
  **`/agent <name>`** (user สั่งเอง รันขนานกับ main ไม่เข้า history),
  **Agent Team** (หลาย process, mailbox + task queue, แต่ละคนมี
  worktree ของตัวเอง) ดู [บทที่ 15](ch15-subagents.md) และ
  [บทที่ 17](ch17-agent-teams.md)
- **Hire-able as a working agent** — ทิศกลับของ orchestration: thClaws
  เป็น *worker* ให้ orchestrator ตัวอื่น (เช่น Paperclip / thcompany)
  จ้างไปทำงาน ทั้งแบบ **Employee** (`thclaws_local` — process บนเครื่อง
  เดียวกัน) และ **Freelancer** (`thclaws_pod` — pod แยก รันบน VPS / cloud
  ก็ได้) orchestrator พูดผ่าน HTTP API เดียวกับที่ user/IDE ใช้
  ([บทที่ 22](ch22-paperclip-adapter.md))
- **จำสิ่งที่สำคัญในระยะยาว 3 ระดับ** —
  **`AGENTS.md` (หรือ `CLAUDE.md`)** ในโปรเจกต์ โดนฉีดเข้า prompt อัตโนมัติ
  ([บทที่ 8](ch08-memory-and-agents-md.md));
  **memory store** ที่ `~/.config/thclaws/memory/` เก็บข้อเท็จจริงที่ agent
  เรียนรู้เกี่ยวกับตัวคุณและโปรเจกต์;
  **KMS (knowledge bases)** สำหรับเอกสารยาว ๆ ที่มี search + reconcile +
  หลายหน้า แนบเข้า session ได้ (`/kms attach`) — ทั้งหมดเป็น markdown
  ที่คุณอ่าน แก้ไข หรือ commit ได้ ([บทที่ 9](ch09-knowledge-bases-kms.md))
- **ประกอบ agent เองจาก building block** —
  **Skill** ([บทที่ 12](ch12-skills.md)) สำหรับ workflow ที่ใช้ซ้ำได้,
  **MCP server** ([บทที่ 14](ch14-mcp.md)) สำหรับเสียบ tool ภายนอก
  (GitHub, DB, Browser, Slack ฯลฯ),
  **Plugin** ([บทที่ 16](ch16-plugins.md)) สำหรับแพ็กทุกอย่างรวมกัน,
  **Knowledge base** ([บทที่ 9](ch09-knowledge-bases-kms.md)) สำหรับ
  wiki ที่ agent ค้น/อ่าน/เขียนเอง พร้อม `/dream` ที่ consolidate KMS
  จาก session ล่าสุดให้อัตโนมัติ
- **รองรับหลาย provider อย่างเท่าเทียม** — Anthropic (native + Claude
  Agent SDK), OpenAI (Chat + Responses/Codex), Google Gemini & Gemma,
  Alibaba DashScope (Qwen), DeepSeek, Z.ai (GLM Coding Plan), NVIDIA
  NIM, NSTDA Thai LLM (OpenThaiGPT, Typhoon, Pathumma, THaLLE),
  OpenRouter, Agentic Press, Azure AI Foundry, Ollama (local +
  Anthropic-compat + Cloud), LMStudio และ slot OpenAI-compatible
  ทั่วไป (`oai/*`) — สลับกลางคันด้วย `/model` หรือ `/provider` ได้
  ([บทที่ 6](ch06-providers-models-api-keys.md))
- **API พร้อมใช้กับเครื่องมือมาตรฐาน** — `--serve` เปิดทั้ง
  `/v1/chat/completions` (OpenAI-compatible สำหรับ Cursor, Aider, n8n,
  openai-python) และ `/agent/run` + `/v1/agent/info` (thClaws-native
  สำหรับ orchestrator เช่น thcompany) — agent ตัวเดียวให้บริการได้
  ทั้งคนและซอฟต์แวร์พร้อมกัน
- **Async webhook delivery** — งานที่รันยาว (deploy, build, multi-step
  research) ส่ง prompt + `x_callback` แล้วปิด connection ได้ thClaws
  จะ POST ผลกลับเมื่อทำเสร็จ ทนต่อ network blip และ orchestrator pod
  restart ระหว่างทาง
- **Plan mode** — สำหรับงานหลาย step ให้ agent `EnterPlanMode` แล้ว
  เสนอ step ทีละขั้น ให้ *คุณ* review และอนุมัติก่อนรัน แต่ละ step
  มี retry budget; failure จะหยุด chain เพื่อให้คุณตัดสินใจต่อ
  ([บทที่ 18](ch18-plan-mode.md))
- **Schedule + cron + watchWorkspace** — `/schedule add` รัน agent
  ตาม cron, fixed interval หรือเมื่อ directory เปลี่ยน มีทั้ง manual,
  in-process scheduler และ daemon (launchd / systemd-user) ที่อยู่
  รอดได้ข้าม reboot ([บทที่ 19](ch19-scheduling.md))
- **Long-running loops** — `/loop` สำหรับ iterate ตาม interval,
  `/goal` สำหรับ audit-driven completion (ทำต่อจนกว่า audit prompt
  จะตอบว่า "เสร็จ" หรือชน budget) ผสม `/goal --auto` ได้สำหรับ
  Ralph-style overnight builder
- **Document workflow** — read + edit + create PDF, DOCX, PPTX, XLSX
  ในตัว plus image rendering — ingest PDF 50 หน้าเข้า KMS แล้วผลิต
  PowerPoint ออกมาในการสนทนาเดียวได้
- **Hooks** — รัน shell script บน lifecycle event ของ agent (8 event
  รวม pre/post_tool_use, permission_denied, session_start,
  pre_compact ฯลฯ) audit ทุก Bash, gate Edit/Write ผ่าน linter,
  แจ้ง Slack เมื่อ session ยาว ๆ จบ ([บทที่ 13](ch13-hooks.md))
- **เหมาะกับทุกสายงาน ไม่ใช่แค่วิศวกร** — Chat tab แบบ streaming สำหรับ
  นักวิจัย PM ฝ่ายกฎหมาย/การตลาด Terminal REPL สำหรับวิศวกร — ใช้
  session และ config ชุดเดียวกัน สลับไปมาได้โดยไม่เสีย context
  ([บทที่ 4](ch04-desktop-gui-tour.md))
- **native บนเครื่อง คุมข้อมูลเอง** — Rust binary ตัวเดียว ไม่ต้องมี service
  เบื้องหลัง ไม่ต้อง cloud จะรันกับ Ollama แบบ offline ล้วน ๆ ก็ได้
- **รันได้ทุกแพลตฟอร์ม** — binary ตัวเดียวกันรันบน macOS (Apple Silicon
  + Intel), Windows, Linux ได้ ใส่ใน Docker container เพื่อ deploy ขึ้น
  VPS / cloud / Kubernetes ก็ได้ — code ตัวเดียวรองรับตั้งแต่ laptop
  ส่วนตัวจนถึง pod บน cluster
- **ยึดมาตรฐานเปิด ไม่ผูกกับ vendor** — ใช้
  [MCP](https://modelcontextprotocol.io/) สำหรับ tool,
  [`AGENTS.md`](https://agents.md) สำหรับ instruction (มาตรฐานที่
  Google, OpenAI, Cursor, Sourcegraph, Factory ใช้), `SKILL.md` สำหรับ
  workflow และ `.mcp.json` สำหรับตั้งค่า MCP server — config
  ขนย้ายไปใช้กับเครื่องมืออื่นที่พูดมาตรฐานเดียวกันได้
- **ความปลอดภัยมาก่อน** — filesystem sandbox จำกัดขอบเขตของ tool
  ไฟล์อยู่ที่ working directory tool ที่เปลี่ยนสถานะต้อง approve
  (ยกเว้นจะตั้ง auto-approve เอง) API key เก็บใน OS keychain หรือ
  `.env` ตามที่คุณเลือกตอนเปิดใช้ครั้งแรก permission request จะติดป้าย
  ว่า agent ตัวไหนกำลังขอ (main, side-channel หรือ subagent)
  ป้องกันการ approve ผิดตัว ([บทที่ 5](ch05-permissions.md))
- **ค่าใช้จ่ายโปร่งใส** — model catalogue ในตัวเก็บราคา per-token-type
  (input / output / cached read / cache write / reasoning) sync จาก
  [LiteLLM](https://github.com/BerriAI/litellm) อัตโนมัติ usage block
  ของทุก turn แสดงครบ orchestrator/UI คำนวณ cost ในเครื่องได้โดยไม่
  ต้องถาม provider
- **Host thClaws ที่ไหนก็ได้** — ใช้บนเครื่องตัวเองได้ หรือ deploy ขึ้น
  [thCompany.ai](https://thcompany.ai) เพื่อให้ thClaws รันบน cloud
  ในชื่อของคุณ — จะถูก *Company จ้าง* (เป็น employee / freelancer ผ่าน
  [บทที่ 22](ch22-paperclip-adapter.md)) หรือยืนเดี่ยวรับงานเองก็ได้
  flow การ deploy มาในรูป plugin host จึงสลับเปลี่ยนได้ ไม่มีการล็อก
  client
- **Session resume** — `thclaws --resume last` ทำงานต่อจาก session
  ล่าสุด, `thclaws --resume <id>` กระโดดไป session ที่ระบุ session
  เก็บเป็น JSONL ที่ `.thclaws/sessions/` — git-friendly, grep-friendly
  ([บทที่ 7](ch07-sessions.md))
- **Settings อยู่ในไฟล์ JSON ไฟล์เดียว** — permission mode, thinking
  budget, allowed/disallowed tool, endpoint ของ provider, KMS ที่แนบไว้,
  max output tokens รวมอยู่ใน `.thclaws/settings.json` (ระดับโปรเจกต์
  commit ลง repo ได้) หรือ `~/.config/thclaws/settings.json` (ระดับ
  ผู้ใช้ทั้งระบบ)
- **Shell escape** — ใส่ `!` นำหน้าบรรทัดใน REPL เพื่อรันคำสั่ง shell
  โดยตรง ไม่เสีย token ไม่มี prompt ขออนุมัติ (เช่น `! git status`)

## สิ่งที่คุณต้องมี

- OS ที่รองรับ: macOS (arm64 หรือ x86_64), Linux (arm64 หรือ x86_64)
  หรือ Windows (arm64 หรือ x86_64)
- API key ของ LLM อย่างน้อยหนึ่งเจ้า — Anthropic, OpenAI, Gemini,
  OpenRouter, Agentic Press, DashScope, DeepSeek, Z.ai, NVIDIA NIM,
  NSTDA Thai LLM หรือ Azure AI Foundry (หรือจะติดตั้ง Ollama /
  LMStudio บนเครื่องเอง ถ้าต้องการใช้แบบ offline)

[บทที่ 2](ch02-installation.md) จะพาติดตั้งและเปิดใช้ครั้งแรก
[บทที่ 6](ch06-providers-models-api-keys.md) อธิบายว่าจะวาง
key ที่ไหนและอย่างไร

## คู่มือเล่มนี้จัดเรียงอย่างไร

คู่มือเล่มนี้ 22 บท จัดเรียงเป็น reference อธิบายวิธีติดตั้งและทุก
ฟีเจอร์ที่ผู้ใช้สัมผัสได้ ทีละเรื่อง พร้อมคำสั่งและการตั้งค่าที่จำเป็น:

**ตั้งค่าและเริ่มต้น**
- [บทที่ 2](ch02-installation.md) — ติดตั้ง
- [บทที่ 3](ch03-working-directory-and-modes.md) — working directory + โหมดการรัน
- [บทที่ 4](ch04-desktop-gui-tour.md) — ทัวร์ Desktop GUI
- [บทที่ 5](ch05-permissions.md) — permissions
- [บทที่ 6](ch06-providers-models-api-keys.md) — providers, models, API keys

**ฟีเจอร์หลัก**
- [บทที่ 7](ch07-sessions.md) — sessions และ resume
- [บทที่ 8](ch08-memory-and-agents-md.md) — memory และ AGENTS.md
- [บทที่ 9](ch09-knowledge-bases-kms.md) — Knowledge bases (KMS) รวมถึง self-improving auto-learn
- [บทที่ 10](ch10-slash-commands.md) — slash commands
- [บทที่ 11](ch11-built-in-tools.md) — built-in tools
- [บทที่ 12](ch12-skills.md) — skills
- [บทที่ 13](ch13-hooks.md) — hooks
- [บทที่ 14](ch14-mcp.md) — MCP

**ประกอบ agent ขั้นสูง**
- [บทที่ 15](ch15-subagents.md) — subagents
- [บทที่ 16](ch16-plugins.md) — plugins
- [บทที่ 17](ch17-agent-teams.md) — agent teams
- [บทที่ 18](ch18-plan-mode.md) — plan mode
- [บทที่ 19](ch19-scheduling.md) — scheduling
- [บทที่ 20](ch20-research.md) — `/research` (background research)

**เข้าถึงจากที่อื่น**
- [บทที่ 21](ch21-line-and-browser-chat.md) — LINE chat + browser bridge
- [บทที่ 22](ch22-paperclip-adapter.md) — Paperclip adapter (จ้าง thClaws ไปทำงานใน orchestrator)

ถ้าเพิ่งเริ่ม อ่านบทที่ 2 ต่อได้เลย ถ้าย้ายมาจาก Claude Code แนะนำให้
ข้ามไปบทที่ 6, 7, 11 และ 13 ถ้าคุ้นเคยพื้นฐานแล้วและสนใจของใหม่ ฟีเจอร์
ที่เพิ่งเพิ่มเข้ามาอยู่ในบทที่ 9 (auto-learn และ `/dream`), บทที่ 15
(`/agent` side-channels), บทที่ 21 (LINE) และบทที่ 22 (Paperclip adapter)
