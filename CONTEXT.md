# CONTEXT — velo (อ่านก่อนแก้โค้ดทุกครั้ง)

## สถาปัตยกรรม
- ภาษาเดียวของโปรเจกต์คือ Rust (edition 2021) — ห้ามเพิ่มโค้ด Go/ภาษาอื่น
- zero dependencies: ใช้ std เท่านั้น ห้ามเพิ่ม crate ถ้าเขียนเองไม่กี่บรรทัดได้
- โครง: `value.rs` (Value+JSON) · `lexer.rs` · `parser.rs` (compile → `Expr` tree + const folding) · `store.rs` · `router.rs` · `http.rs` (HTTP/1.1 เขียนเอง) · `main.rs` (CLI)
- row ทุกแถวเก็บ JSON bytes ไว้ข้างโครงสร้าง (`Value::Row`) + collection cache JSON ของ list ทั้งก้อน (`Value::Raw`) → ต้อง `invalidate()` ทุกครั้งที่เขียน ไม่งั้นข้อมูลค้าง
- route ที่ pure (ไม่มี db/body/param) ถูก fold เป็น bytes ตอน compile → response path แค่ copy
- เครื่องนี้ไม่มี gcc → build ผ่าน target `x86_64-unknown-linux-musl` + `rust-lld` (ตั้งใน `.cargo/config.toml`)
- I/O = epoll event loop 1 instance ต่อ worker thread (`VELO_WORKERS` default = cores) แชร์ listener ด้วย `EPOLLEXCLUSIVE` · socket nonblocking · เรียก epoll ผ่าน `extern "C"` ใน `epoll.rs` (ไม่ใช้ crate libc)
- ห้ามกลับไปใช้ thread-ต่อ-connection · state ต่อ conn อยู่ใน `Conn` (inbuf/out/body reuse) ตัด idle ด้วย sweep ทุก 1 วิ ตาม `VELO_KEEPALIVE`

- persistence = snapshot ทั้งไฟล์ (atomic tmp+rename) trigger ด้วย dirty flag ใน `Store` ไม่ใช่ WAL · เปิดด้วย `--data`/`VELO_DATA` เท่านั้น default = in-memory

## สไตล์โค้ด
- ห้าม comment code
- ห้ามใช้อีโมจิทุกที่ (โค้ด, README, commit message)
- buffer ต้อง reuse ต่อ connection ห้าม allocate ต่อ request ถ้าเลี่ยงได้

## ต้องทำเสมอ
- ทุกงานที่เสร็จ: `cargo test` ผ่าน → bench ถ้าแตะ hot path → อัปเดต README (เวอร์ชัน + ตัวเลข) → commit & push `thanadon-dev/velo` เอง
- ขึ้นเวอร์ชันใน `Cargo.toml` แล้ว README/examples ต้องตรงกัน
- bench ด้วย `./bench.sh` (ใช้ `ab -k`) เก็บตัวเลขลง README
