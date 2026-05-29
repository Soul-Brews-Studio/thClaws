# บทที่ 21 — LINE chat & web browser bridge

ขับ thClaws จากมือถือ — ไม่ว่าจะเป็นการคุยผ่าน LINE (ผ่าน OA bot
`@thClaws`) หรือใช้หน้า chat ในเว็บเบราว์เซอร์ตัวไหนก็ได้ ทั้งสอง
ช่องทางใช้ Rust agent loop เดียวกันบน desktop ของคุณ เปลี่ยน
แค่ surface เท่านั้น เพิ่มมาตั้งแต่ v0.9.0+ ผ่านชุด plan-07 /
plan-08 / plan-10

## ทำไมต้องใช้

- approve คำสั่ง `Bash` จากมือถือ ขณะที่ desktop รันอัตโนมัติ
  อยู่บ้าน
- คุยกับ agent ต่อจากที่ไหนก็ได้ — พิมพ์บนมือถือ ส่วน tool
  registry เต็มชุดบน desktop (Bash, Edit, KMS, MCP, skills) ยัง
  รันในเครื่อง
- สั่งงานยาว ๆ โดยไม่ต้องนั่งติดโต๊ะ

desktop ไม่หายไปไหน — code, secret, tool ทั้งหมดยังอยู่ในเครื่อง
มือถือ/เบราว์เซอร์เป็นแค่ bridge สำหรับอ่าน+พิมพ์เท่านั้น

## ทำงานยังไง (พารากราฟเดียว)

Axum service เล็ก ๆ ที่ `line.thclaws.ai` (และ `chat.thclaws.ai`
สำหรับ browser variant) ถือ WebSocket จาก desktop ของคุณไว้
แล้ว route LINE inbound message / browser keystroke เข้าไปให้
desktop รัน agent ตามปกติและ fan out ทุก assistant delta, tool
call, approval prompt กลับผ่าน WS เดิม — มือถือหรือเบราว์เซอร์
เห็น conversation ที่ stream ออกมาเรียลไทม์ session ฝั่ง LINE
ผูกกับ LINE user id, session ฝั่งเบราว์เซอร์ auth ด้วย magic
link ที่ LINE bot mint ให้

## pair มือถือ (LINE)

setup ครั้งเดียว:

1. **เพิ่ม LINE OA** — scan QR ที่
   [`thclaws.ai/line`](https://thclaws.ai/line) (หรือ search
   `@thClaws` ใน LINE)
2. ใน thClaws เปิด Settings → **LINE** → **Pair phone** modal
   จะแสดง code 6 ตัวอักษร (เช่น `KJ4-9P2`)
3. ส่ง code นั้นให้ LINE OA bot จะตอบ "Paired ✓ as
   *<line-display-name-ของคุณ>*"
4. chip LINE บน sidebar เปลี่ยนเป็นเขียว — เชื่อมต่อแล้ว

หลัง pair ทุก message ที่ส่งให้ `@thClaws` จะไหลเข้า chat
session ของ thClaws บน desktop agent รันที่ฝั่งนั้น stream
response กลับมา แล้ว LINE bot relay มาเป็น bubble tool call ที่
ต้อง approve (Bash, Edit, Write) จะ trigger LINE Quick Reply
chip — แตะ **[Approve]** หรือ **[Deny]** จากมือถือ

## คำสั่ง LINE OA

หลัง pair แล้ว LINE bot รู้จัก text command ชุดเล็ก ๆ ที่เหลือ
ถือเป็น chat message

| คุณพิมพ์ | เกิดอะไรขึ้น |
|---|---|
| `/chat` | mint magic link ไปยัง browser chat (ดูข้างล่าง) แล้วตอบกลับมา link ใช้ครั้งเดียว, TTL 10 นาที |
| `/pair` | ออก pairing code ใหม่ — มีประโยชน์ตอน disconnect thClaws แล้วอยากเริ่ม session ใหม่ |
| `/unpair` | ลืม LINE user id นี้ message ถัดไปจะได้ pairing code ใหม่ ไม่ใช่ chat |
| `/status` | บอกว่า thClaws ติดต่อจาก relay ได้อยู่ไหมตอนนี้ |
| อย่างอื่น | route ไปเป็น user message ตามปกติให้ chat session บน desktop |

ถ้า pair แล้วแต่ desktop offline (พับ laptop, เน็ตหลุด) bot จะ
ตอบ "thClaws is offline" แทนที่จะกลืน message เงียบ ๆ

## browser chat (เส้นทาง `/chat`)

LINE bubble เหมาะกับ approve สั้น ๆ และ prompt เร็ว ๆ แต่อ่าน
code block, response ยาว ๆ, markdown ลำบาก ส่ง `/chat` ให้ OA
แล้วจะได้ magic link กลับมา:

```
https://chat.thclaws.ai/launch?token=...
```

เปิดในเบราว์เซอร์ตัวไหนก็ได้ — link auto-redirect ผ่านหน้า
splash (มีไว้เลี่ยง URL-preview crawler ของ LINE ที่จะใช้ token
ก่อนคุณแตะ) หลัง redirect คุณจะตกลงบน chat surface เต็มรูปแบบ:

- sidebar แสดง session id, ปุ่ม sign-out, และ indicator
  "browser connected" ที่ฝั่ง desktop
- assistant response render เป็น markdown พร้อม code block ที่
  syntax-highlight (ผ่าน marked.js + DOMPurify ที่ vendor มา —
  render ทั้งหมดอยู่ในเบราว์เซอร์ ไม่มี remote loader ไม่มี
  eval)
- history replay อัตโนมัติตอน connect — reconnect กลางคันก็
  resume จากจุดเดิม (~50 message ล่าสุด ส่งจาก Redis stream บน
  relay)
- tool approval เปิด modal inline พร้อมปุ่ม **[Approve] [Deny]**
  แทนที่จะ route ไปที่ LINE Quick Reply
- session หมดอายุหลัง idle 10 นาที — reconnect fail 3 ครั้งติด
  trigger splash "session expired" ชี้กลับไปที่ `/chat` ใน LINE
  เพื่อขอ link ใหม่

browser link เป็น **per-session, single-use, HTTPS-only,
HttpOnly cookie** การแชร์ link เท่ากับยื่น session บน desktop
ให้คนอื่น — อย่าทำ

## rich-menu shortcut (v0.9.3+)

ถ้ามือถือคุณแสดง rich menu ของ LINE OA (toolbar ด้านล่างพร้อม
ปุ่ม custom) จะมีปุ่ม pin ไว้ 2 ปุ่ม:

- **Chat** — เทียบเท่าพิมพ์ `/chat` แตะครั้งเดียวก็ได้ magic
  link ไปยัง browser chat
- **Pair** — เทียบเท่าพิมพ์ `/pair` ใช้ออก pairing code ใหม่
  ไวสุดเวลา disconnect

operator ที่ deploy LINE OA เองสามารถติดตั้ง rich menu ด้วย
script `dev-plan/08-line-server-k3s/rich-menu-setup.sh` — อ่าน
[`docs/line-rich-menu-setup.md`](../../docs/line-rich-menu-setup.md)
สำหรับ walk-through เต็มชุด

## approve จากมือถือหรือเบราว์เซอร์

ตอน LINE bridge connect runtime permission mode จะเป็น `linegated`
(ดู[บทที่ 5](ch05-permissions.md)) และ **ทุก approval request จะ
route ไปที่ LINE ไม่ว่าจะพิมพ์มาจาก surface ไหน** — Terminal tab,
Chat tab, REPL หรือ LINE bubble ก็ตาม approver เป็น process-wide
singleton ไม่รู้ว่า tool call มาจาก surface ไหน — ขณะ pair อยู่
phone คือ approval inbox เดียว

- **browser chat (`/chat`) เปิดอยู่:** modal approval โผล่ขึ้นมา
  ใน browser พร้อมชื่อ tool, preview argument เต็ม ๆ, และปุ่ม
  **[Approve] [Deny]** — UX ดีกว่าให้ Quick Reply chip ของ LINE
  preview argument ยาว ๆ approve ที่ surface ไหนก็ปิดทั้งสอง
- **browser chat ไม่เปิด (หรือยังไม่ได้ mint):** fall back ไปที่
  LINE OA Quick Reply bot push bubble หน้าตาแบบนี้:

  ```
  thClaws wants to run:
    bash -c "ls -la ~/Downloads"

  [Approve]  [Deny]
  ```

  แตะ chip; คำตอบไหลกลับไป desktop ภายใน ~1 วินาที

**bypass ตอน pair อยู่** ถ้าไม่อยากให้ approval route ไปมือถือ:

- `/permissions auto` — override `linegated` ทำให้ mutating tool
  รันได้โดยไม่ขออนุญาต persist ลง `settings.json` ยืนข้าม LINE
  disconnect/reconnect
- disconnect LINE ที่ Settings → LINE Connect — restore pre-LINE
  mode (ปกติคือ `auto` หรือ `ask`) ทันที

ตอน LINE **ไม่ได้** pair modal Approval บน desktop ทำงานปกติ —
routing มือถือ/เบราว์เซอร์ active เฉพาะตอน bridge connect เท่านั้น

## upload ไฟล์จากมือถือหรือเบราว์เซอร์

attach ไฟล์จาก surface ไหนก็ได้ — desktop บันทึกไฟล์ลง
`<workspace>/uploads/` แล้ว `AGENT.md` ในไดเรกทอรีนั้นบอก agent
ว่าควรทำอะไรกับไฟล์ เพิ่มใน v0.9.6

**ลิมิต:**
- **25 MB ต่อไฟล์** (`UPLOAD_MAX_BYTES`)
- **5 ไฟล์ต่อ message** (`UPLOAD_MAX_FILES`)
- MIME type อะไรก็ได้ — text, image, PDF, archive desktop ไม่
  unpack ไม่ transform แค่ land byte ลง workspace

**ชื่อไฟล์ชน** จะ resolve ด้วยการเติม `_n` ก่อน extension ถ้า
upload `notes.md` แล้วมี `notes.md` อยู่แล้ว ไฟล์ที่สองจะลงเป็น
`notes_1.md`; ตัวที่สามเป็น `notes_2.md` ชื่อ original
preserve ไว้ (ยกเว้น sanitise path-traversal — `../../etc/passwd`
ลงเป็น `passwd`)

**จาก browser chat** (`chat.thclaws.ai`): drag-and-drop ไฟล์ลง
บน chat surface หรือคลิก icon paperclip ข้างช่อง composer
desktop ส่ง synthetic chat message อธิบาย upload (ชื่อไฟล์ +
ขนาด) ตามด้วยบรรทัด directive `Read the file and respond.`
agent จึงตีว่า drop เป็นคำขอให้ลงมือทำกับ content — ไม่ใช่แค่
FYI (ก่อน v0.9.7 synthetic เป็นข้อมูลล้วน บาง model จะตอบ
"อยากให้ทำอะไรกับสิ่งนี้?") project-level `AGENT.md` /
`CLAUDE.md` override directive ได้ ถ้านั่นคือพฤติกรรมที่ต้องการ

**จาก LINE:** ส่งไฟล์เป็น LINE attachment ปกติ (รูป, วิดีโอ,
ไฟล์) relay forward upload reference ผ่าน broker channel;
desktop ดึง byte จาก CDN ของ LINE ด้วย channel access token
แล้ว save ในเครื่อง agent จึงเห็น synthetic message รูปแบบ
เดียวกับ path ของ browser รวมถึง directive `Read the file and
respond.` ตัวเดียวกัน

**คุม behavior ที่ไหน:** วาง `AGENT.md` ไว้ที่
`<workspace>/uploads/AGENT.md` (หรือที่ workspace root ถ้าอยาก
กฎเดียวสำหรับทุกอย่าง) agent อ่านเป็นส่วนหนึ่งของ cascade
CLAUDE.md / AGENT.md ปกติ แล้วทำตาม directive ที่อยู่ในนั้น:
"OCR PDF ที่ upload ทุกตัวแล้วเก็บ text ใน `kms/sources/`",
"auto-rename screenshot จาก `Photo 2026-…` เป็น slug", ฯลฯ ถ้า
ไม่มี `AGENT.md` ไฟล์จะนิ่งรอจนคุณบอก agent ขั้นต่อไป

## privacy และ trust boundary

- **desktop ไม่ proxy upstream LLM call ผ่าน relay**
  prompt ของคุณวิ่งจาก desktop ตรงไปยัง Anthropic / OpenAI /
  ฯลฯ relay carry แค่ user-facing message ระหว่าง surface กับ
  desktop เท่านั้น
- **relay เห็น content message ระหว่างทาง** (ต้องเห็นเพื่อ
  route) ถ้าไม่อยากให้ third-party อ่าน prompt host เองได้ —
  binary relay อยู่ที่ `crates/line-server/` ใน workspace fork;
  ตัว public OSS ไม่ ship distribution นี้ อ่าน plan-08 ใน
  `dev-plan/` ของ workspace สำหรับ k3s deployment shape
- **token / API key ไม่ออกจาก desktop** relay ถือ LINE channel
  secret หนึ่งตัว (ไว้ verify signature) และ Postgres user
  profile cache (ชื่อ + LINE user id) ต่อ paired user — แค่นั้น
- **LINE pairing token ใช้ครั้งเดียว, TTL 10 นาที, hash ฝั่ง
  server** pairing code ที่ถูกขโมยไปใช้ไม่ได้แล้วหลัง OA emit
  reply "Paired ✓"

## troubleshooting

| อาการ | สาเหตุที่น่าจะใช่ | วิธีแก้ |
|---|---|---|
| link `/chat` ขึ้น "expired" ตั้งแต่แตะครั้งแรก | URL preview crawler ของ LINE กิน token ไปก่อน | เปิด link จาก LINE chat ตรง ๆ ไม่ใช่จาก copy ที่ forward ต่อ |
| LINE bot ตอบ "thClaws is offline" | WS ฝั่ง desktop หลุด (sleep, network) | เปิด desktop กลับมา; pairing ยังอยู่ |
| browser chat ค้างที่ "Opening thClaws Chat…" | เบราว์เซอร์ block script auto-submit inline | check ว่า CSP อนุญาต `script-src 'self' 'unsafe-inline'` บน `/launch` |
| ปุ่ม Quick Reply ของ LINE ไม่ขึ้นตอน approval | browser chat เปิดอยู่ด้วย — approval ไปที่นั่นแทน | approve ในเบราว์เซอร์ หรือปิด tab แล้ว approval รอบหน้าจะตกไปที่ LINE |
| pairing code ค้าง "(none)" หลังพิมพ์ส่ง | code เกิน 10 นาที หรือถูกใช้ไปแล้ว | เปิด Pair modal อีกครั้งเพื่อ mint code ใหม่ |
| pill "browser connected" ไม่โผล่บน desktop | TTL ของ magic link token หมดก่อนคุณเปิด | ส่ง `/chat` จาก LINE ใหม่เพื่อขอ link ใหม่ |

## status command บน desktop

`make line-status` (จาก workspace root) print ตาราง status ต่อ
user โดย join Postgres profile กับ Redis presence flag —
มีประโยชน์สำหรับ operator ที่ run LINE relay ของตัวเอง:

```
$ make line-status
user_id            paired  present  browser  last_seen
U1a2b3...          ✓       ✓        -        2 min ago
U9z8y7...          ✓       -        -        3 days ago
```

`paired` = เคย pair, `present` = WS connect อยู่ตอนนี้,
`browser` = `/chat` browser session active, `last_seen` =
webhook activity ล่าสุด

## ไม่อยู่ในบทนี้

- internal architecture (broker channel multiplex, WS protocol,
  Redis stream layout) — ดู
  [`line-bridge.md`](../../thclaws-technical-manual/line-bridge.md)
  ใน technical manual
- ตั้ง LINE OA ตั้งแต่ต้น (channel secret, webhook URL, ติดตั้ง
  rich menu) — เรื่องฝั่ง operator อ่านได้ที่
  [`docs/line-rich-menu-setup.md`](../../docs/line-rich-menu-setup.md)
  และ doc plan-08 ใน workspace
- cloud gateway (paid SaaS proxy) — ship ใน v0.9.6 ในชื่อ
  `gateway.thclaws.ai` ดู[บทที่ 6](ch06-providers-models-api-keys.md)
  สำหรับ sign-in + toggle ต่อ provider ที่ฝั่ง user, และ
  [`provider-thclaws-gateway.md`](../../thclaws-technical-manual/provider-thclaws-gateway.md)
  ใน technical manual สำหรับ wire shape
