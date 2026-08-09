# บทที่ 22 — Paperclip adapter (ยกเลิกแล้ว)

**บทนี้ถูกถอดออกใน v0.110.0** เดิมอธิบายวิธีใช้
`@thclaws/paperclip-adapter` ซึ่งเป็น npm package ที่ทำให้
[Paperclip](https://paperclip.ai) จ้าง thClaws ไปเป็น runtime ตัวหนึ่งได้
ผลิตภัณฑ์ที่ adapter ตัวนี้รองรับถูกยกเลิกไปแล้ว และซอร์สของมันไม่ได้อยู่
ใน repository นี้อีกต่อไป

หน้านี้เก็บไว้เป็น stub เพื่อไม่ให้ลิงก์เดิมเสีย

## ถ้าคุณมาที่นี่เพราะอยากสั่ง thClaws จากระบบอื่น

ยังทำได้อยู่ครับ — และไม่เคยผูกกับ Paperclip ตั้งแต่แรก thClaws เปิด
HTTP surface สองชุดเมื่อรัน `thclaws --serve` ซึ่ง orchestrator, scheduler
หรือ CI job อะไรก็เรียกได้:

- **`POST /agent/run`** — รูปแบบดั้งเดิมของ thClaws รับ prompt กับ
  `workspace_dir` (ใส่หรือไม่ใส่ก็ได้) แล้วรัน bootstrap ครบชุดทั้ง
  skill / MCP / plugin / policy โดยผูกกับ directory นั้น และ stream
  event จริงของ thClaws (tool call, skill invocation) ออกมา ไม่ใช่แกล้ง
  ทำเป็น token แบบ OpenAI รองรับคุยต่อหลายเทิร์นผ่าน `session_id` และ
  ส่งผลลัพธ์แบบ fire-and-forget ผ่าน `x_callback`
- **`POST /v1/chat/completions`** — เข้ากันได้กับ OpenAI สำหรับ client
  ที่พูดโปรโตคอลนั้นอยู่แล้ว (Cursor, Aider, n8n, `openai-python`)

ส่วน `GET /v1/agent/info` จะบอกว่า daemon ที่รันอยู่มีอะไรบ้าง — skills,
MCP server, model catalogue, เวอร์ชัน — orchestrator จึงแสดง capability
ที่ตัวเองไม่ได้เป็นคนใส่เข้าไปได้

อ้างอิงเต็ม: `agent-endpoint.md`, `openai-api.md` และ
`agent-info-endpoint.md` ใน technical manual

## ถ้าคุณใช้ npm package อยู่

package ยังอยู่บน npm และติดตั้งได้ แต่ไม่มีคนดูแลแล้ว และไม่ได้ถูก build
หรือทดสอบจาก repository นี้อีก แนะนำให้ย้ายไปเรียก `POST /agent/run`
โดยตรง — ได้ความสามารถเดียวกันโดยไม่ต้องมีตัวห่อ
