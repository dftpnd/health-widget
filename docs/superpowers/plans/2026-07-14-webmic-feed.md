# Webmic Feed Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Со страницы веб-микрофона можно слать текст и картинки; виджет показывает их в нижней секции ресайзабельного окна `hw-web`.

**Architecture:** Два сырых POST-эндпоинта (`/api/msg`, `/api/img`) кладут посты в `Shared.posts` (кап 30, картинки декодятся и даунскейлятся до 1600×1600 на приёме). Окно `hw-web` становится ресайзабельным с персистом размера (`web_w`/`web_h`) и делится на секции «🗣 Речь» / «📥 Присланное»; картинки рисуются кэшированными текстурами. Страница получает поле ввода, кнопку 📎 и обработчик Ctrl+V.

**Tech Stack:** Rust (tiny_http vendored, image 0.25 — уже в дереве через eframe), egui 0.31, React/Vite.

## Global Constraints

- Спека: `docs/superpowers/specs/2026-07-14-webmic-feed-design.md`.
- Никаких комментариев в коде (CLAUDE.md).
- Ветка `master`, коммит на задачу, `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Лимиты: текст 64 КБ (`413`), картинка 8 МБ (`413`), тип не png/jpeg/webp или не декодится (`415`), пустой текст (`400`); кап ленты 30.
- Токен-правило не трогаем: `needs_token` уже покрывает `/api/*`.

---

### Task 1: Сервер — модель Post и POST /api/msg

**Files:**
- Modify: `src/webmic.rs`

**Interfaces:**
- Produces: `pub enum Post { Text(u64, String), Image(u64, egui::ColorImage) }`; поля `Shared.posts: VecDeque<Post>`, `Shared.next_post_id: u64`; `Shared::push_post(impl FnOnce(u64) -> Post)`; `fn msg_from(&[u8]) -> Result<String, u16>`; `fn respond_code(req, code: u16)`. Task 3 читает `Shared.posts`.

- [ ] **Step 1: Падающие тесты** в `mod tests`:

```rust
    #[test]
    fn msg_parsing() {
        assert_eq!(msg_from("привет".as_bytes()), Ok("привет".to_string()));
        assert_eq!(msg_from(b"  \n "), Err(400));
        assert_eq!(msg_from(&[0xff, 0xfe]), Err(400));
        assert_eq!(msg_from(&vec![b'a'; MSG_MAX + 1]), Err(413));
    }

    #[test]
    fn posts_capped_at_30() {
        let mut sh = Shared::default();
        for i in 0..31 {
            sh.push_post(|id| Post::Text(id, format!("m{i}")));
        }
        assert_eq!(sh.posts.len(), 30);
        assert!(matches!(sh.posts.front(), Some(Post::Text(1, _))));
        assert!(matches!(sh.posts.back(), Some(Post::Text(30, _))));
    }
```

- [ ] **Step 2: Проверить красное**: `cargo test webmic 2>&1 | tail -3` → ошибка компиляции (нет `msg_from`/`Post`).

- [ ] **Step 3: Реализация.**

```rust
pub const MSG_MAX: usize = 64 * 1024;
const POSTS_CAP: usize = 30;

pub enum Post {
    Text(u64, String),
    Image(u64, egui::ColorImage),
}
```

в `Shared` — поля `pub posts: VecDeque<Post>` и `next_post_id: u64`, метод:

```rust
    pub fn push_post(&mut self, make: impl FnOnce(u64) -> Post) {
        self.next_post_id += 1;
        if self.posts.len() >= POSTS_CAP {
            self.posts.pop_front();
        }
        self.posts.push_back(make(self.next_post_id));
    }
```

```rust
fn msg_from(body: &[u8]) -> Result<String, u16> {
    if body.len() > MSG_MAX {
        return Err(413);
    }
    let text = std::str::from_utf8(body).map_err(|_| 400u16)?.trim().to_string();
    if text.is_empty() {
        return Err(400);
    }
    Ok(text)
}

fn respond_code(req: tiny_http::Request, code: u16) {
    let body = if code == 200 { r#"{"ok":true}"# } else { r#"{"ok":false}"# };
    let resp = tiny_http::Response::from_string(body)
        .with_status_code(code)
        .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
    let _ = req.respond(resp);
}
```

в `handle_request` после ветки `/api/audio`:

```rust
    if req.method() == &tiny_http::Method::Post && path == "/api/msg" {
        let mut body = Vec::new();
        let _ = req.as_reader().take(MSG_MAX as u64 + 1).read_to_end(&mut body);
        let code = match msg_from(&body) {
            Ok(text) => {
                if let Ok(mut g) = shared.lock() {
                    g.push_post(|id| Post::Text(id, text));
                }
                200
            }
            Err(c) => c,
        };
        respond_code(req, code);
        return;
    }
```

- [ ] **Step 4: Зелёное**: `cargo test 2>&1 | tail -3` → ok.
- [ ] **Step 5: Commit** `feat(webmic): POST /api/msg — текст со страницы в ленту Shared.posts`.

---

### Task 2: Сервер — POST /api/img с декодом и даунскейлом

**Files:**
- Modify: `Cargo.toml` (прямая зависимость `image = "0.25"`)
- Modify: `src/webmic.rs`

**Interfaces:**
- Consumes: `Shared::push_post`, `respond_code` из Task 1.
- Produces: `fn img_format(&str) -> Option<image::ImageFormat>`; `fn fit(u32, u32, u32) -> (u32, u32)`; `fn decode_image(&[u8], &str) -> Result<egui::ColorImage, u16>`.

- [ ] **Step 1: Падающие тесты**:

```rust
    #[test]
    fn img_format_from_content_type() {
        assert_eq!(img_format("image/png"), Some(image::ImageFormat::Png));
        assert_eq!(img_format("image/jpeg; charset=utf-8"), Some(image::ImageFormat::Jpeg));
        assert_eq!(img_format("image/webp"), Some(image::ImageFormat::WebP));
        assert_eq!(img_format("text/html"), None);
    }

    #[test]
    fn fit_downscales_preserving_ratio() {
        assert_eq!(fit(4000, 3000, 1600), (1600, 1200));
        assert_eq!(fit(3000, 4000, 1600), (1200, 1600));
        assert_eq!(fit(800, 600, 1600), (800, 600));
    }

    #[test]
    fn decode_rejects_garbage_and_accepts_png() {
        assert_eq!(decode_image(b"mus", "image/png").err(), Some(415));
        assert_eq!(decode_image(b"x", "text/plain").err(), Some(415));
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let img = decode_image(&png, "image/png").unwrap();
        assert_eq!(img.size, [2, 2]);
    }
```

- [ ] **Step 2: Красное**: `cargo test webmic 2>&1 | tail -3` → нет `img_format`.

- [ ] **Step 3: Реализация.** Cargo.toml `[dependencies]`: `image = "0.25"`. В webmic.rs:

```rust
const IMG_MAX: usize = 8 * 1024 * 1024;
const IMG_FIT: u32 = 1600;

fn img_format(content_type: &str) -> Option<image::ImageFormat> {
    match content_type.split(';').next()?.trim() {
        "image/png" => Some(image::ImageFormat::Png),
        "image/jpeg" => Some(image::ImageFormat::Jpeg),
        "image/webp" => Some(image::ImageFormat::WebP),
        _ => None,
    }
}

fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w, h);
    }
    let k = (max as f64 / w as f64).min(max as f64 / h as f64);
    (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1))
}

fn decode_image(body: &[u8], content_type: &str) -> Result<egui::ColorImage, u16> {
    if body.len() > IMG_MAX {
        return Err(413);
    }
    let fmt = img_format(content_type).ok_or(415u16)?;
    let img = image::load_from_memory_with_format(body, fmt).map_err(|_| 415u16)?;
    let (tw, th) = fit(img.width(), img.height(), IMG_FIT);
    let img = if (tw, th) != (img.width(), img.height()) {
        img.resize(tw, th, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(size, &rgba))
}
```

в `handle_request` после ветки `/api/msg`:

```rust
    if req.method() == &tiny_http::Method::Post && path == "/api/img" {
        let ct = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Content-Type"))
            .map(|h| h.value.as_str().to_string())
            .unwrap_or_default();
        let mut body = Vec::new();
        let _ = req.as_reader().take(IMG_MAX as u64 + 1).read_to_end(&mut body);
        let code = match decode_image(&body, &ct) {
            Ok(px) => {
                if let Ok(mut g) = shared.lock() {
                    g.push_post(|id| Post::Image(id, px));
                }
                200
            }
            Err(c) => c,
        };
        respond_code(req, code);
        return;
    }
```

- [ ] **Step 4: Зелёное**: `cargo test 2>&1 | tail -3` → ok.
- [ ] **Step 5: Commit** `feat(webmic): POST /api/img — декод и даунскейл картинок в ленту`.

---

### Task 3: Виджет — ресайз окна hw-web и секция «Присланное»

**Files:**
- Modify: `src/state.rs` (поля `web_w`, `web_h`)
- Modify: `src/main.rs` (`App`, `show_web_window`, `current_state`)

**Interfaces:**
- Consumes: `webmic::Post`, `Shared.posts`.
- Produces: поля `App.web_spawn_size: egui::Vec2`, `App.web_size: egui::Vec2`, `App.web_textures: std::collections::HashMap<u64, egui::TextureHandle>`; state-поля `web_w: Option<f32>`, `web_h: Option<f32>`.

- [ ] **Step 1: state.rs** — после `web_y` добавить:

```rust
    #[serde(default)]
    pub web_w: Option<f32>,
    #[serde(default)]
    pub web_h: Option<f32>,
```

- [ ] **Step 2: App** — поля после `web_pos`:

```rust
    web_spawn_size: egui::Vec2,
    web_size: egui::Vec2,
    web_textures: std::collections::HashMap<u64, egui::TextureHandle>,
```

константы рядом с `CHAT_WIN_*`:

```rust
const WEB_WIN_W: f32 = 420.0;
const WEB_WIN_H: f32 = 320.0;
```

инициализация в `App::new` рядом с `web_pos`:

```rust
            web_spawn_size: egui::vec2(st.web_w.unwrap_or(WEB_WIN_W), st.web_h.unwrap_or(WEB_WIN_H)),
            web_size: egui::vec2(st.web_w.unwrap_or(WEB_WIN_W), st.web_h.unwrap_or(WEB_WIN_H)),
            web_textures: std::collections::HashMap::new(),
```

в `toggle_webmic` в ветке `Ok(w)` перед копированием ссылки: `self.web_spawn_size = self.web_size;`

в `current_state` рядом с `web_x/web_y`: `web_w: Some(self.web_size.x), web_h: Some(self.web_size.y),`

- [ ] **Step 3: show_web_window** — заменить снапшот и содержимое:

снапшот под локом (вместо текущего кортежа):

```rust
        enum Snap {
            Text(String),
            Image(u64),
        }
        let (lines, partial, stt_on, active, posts, new_imgs) = match shared.lock() {
            Ok(g) => {
                let mut posts = Vec::new();
                let mut new_imgs = Vec::new();
                for p in &g.posts {
                    match p {
                        webmic::Post::Text(_, t) => posts.push(Snap::Text(t.clone())),
                        webmic::Post::Image(id, px) => {
                            if !self.web_textures.contains_key(id) {
                                new_imgs.push((*id, px.clone()));
                            }
                            posts.push(Snap::Image(*id));
                        }
                    }
                }
                (
                    g.lines.iter().cloned().collect::<Vec<_>>(),
                    g.partial.clone(),
                    g.stt_on,
                    g.client_active(),
                    posts,
                    new_imgs,
                )
            }
            Err(_) => return,
        };
        for (id, px) in new_imgs {
            let tex = ctx.load_texture(format!("hwweb-{id}"), px, Default::default());
            self.web_textures.insert(id, tex);
        }
        self.web_textures.retain(|id, _| {
            posts.iter().any(|p| matches!(p, Snap::Image(pid) if pid == id))
        });
```

билдер окна:

```rust
        let vb = egui::ViewportBuilder::default()
            .with_title(winctl::WEB_CAPTION)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(true)
            .with_inner_size([self.web_spawn_size.x, self.web_spawn_size.y])
            .with_min_inner_size([320.0, 280.0]);
```

внутри `CentralPanel` (после хедера): грип как у чата (drag-исключение по grip_rect в `ui.interact`-блоке), затем две секции:

```rust
                let half = (ui.available_height() - 24.0) / 2.0;
                ui.label(section_title("🗣 Речь"));
                egui::ScrollArea::vertical()
                    .id_salt("web-speech")
                    .max_height(half)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| { /* текущий рендер lines/partial */ });
                ui.separator();
                ui.label(section_title("📥 Присланное"));
                egui::ScrollArea::vertical()
                    .id_salt("web-posts")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if posts.is_empty() {
                            ui.label(hint_label("пришли текст или картинку со страницы"));
                        }
                        for p in &posts {
                            match p {
                                Snap::Text(t) => { ui.label(egui::RichText::new(t).size(15.0).color(egui::Color32::from_rgb(205, 210, 220))); }
                                Snap::Image(id) => {
                                    if let Some(tex) = self.web_textures.get(id) {
                                        let size = tex.size_vec2();
                                        let k = (ui.available_width() / size.x).min(1.0);
                                        ui.image((tex.id(), size * k));
                                    }
                                }
                            }
                        }
                    });
                draw_resize_grip(ui, cctx, grip_rect);
```

(`section_title`/`hint_label` — не новые хелперы, а инлайн `RichText` в стиле существующих; в реализации написать по месту.) После `show`: `self.web_size = cctx.screen_rect().size();`

- [ ] **Step 4: Сборка и тесты**: `cargo test 2>&1 | tail -3` → ok; `cargo build --release`.
- [ ] **Step 5: Commit** `feat(webmic): окно hw-web — ресайз с персистом и секция «Присланное»`.

---

### Task 4: Страница — текст, 📎 и Ctrl+V

**Files:**
- Modify: `web/src/App.jsx`, `web/index.html` (стили)

**Interfaces:**
- Consumes: `POST api/msg` (text/plain), `POST api/img` (image/*), токен `TOKEN`.

- [ ] **Step 1: App.jsx.** Состояние и отправка:

```jsx
  const [msg, setMsg] = useState('')
  const fileRef = useRef(null)

  async function post(path, body, type) {
    const r = await fetch(`${path}?t=${TOKEN}`, {
      method: 'POST',
      headers: type ? { 'Content-Type': type } : {},
      body,
    })
    if (!r.ok) throw new Error(String(r.status))
  }

  async function sendText() {
    const text = msg.trim()
    if (!text) return
    try {
      await post('api/msg', text, 'text/plain; charset=utf-8')
      setFinals((f) => [...f, `→ ${text}`])
      setMsg('')
      setErr('')
    } catch {
      setErr('текст не отправился')
    }
  }

  async function sendImage(file, label) {
    if (!file) return
    try {
      await post('api/img', file, file.type || 'image/png')
      setFinals((f) => [...f, `→ 🖼 ${label}`])
      setErr('')
    } catch {
      setErr('картинка не отправилась')
    }
  }

  useEffect(() => {
    const onPaste = (e) => {
      const item = [...(e.clipboardData?.items || [])].find((i) => i.type.startsWith('image/'))
      if (item) sendImage(item.getAsFile(), 'из буфера')
    }
    document.addEventListener('paste', onPaste)
    return () => document.removeEventListener('paste', onPaste)
  }, [])
```

разметка после `.feed`:

```jsx
      <div className="compose">
        <input
          className="msg"
          placeholder="текст на виджет…"
          value={msg}
          onChange={(e) => setMsg(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && sendText()}
        />
        <button className="send" onClick={sendText}>➤</button>
        <button className="send" onClick={() => fileRef.current?.click()}>📎</button>
        <input
          ref={fileRef}
          type="file"
          accept="image/png,image/jpeg,image/webp"
          hidden
          onChange={(e) => {
            sendImage(e.target.files?.[0], e.target.files?.[0]?.name || 'файл')
            e.target.value = ''
          }}
        />
      </div>
```

- [ ] **Step 2: стили в `web/index.html`** после `.partial`:

```css
      .compose { display: flex; gap: 8px; }
      .msg {
        flex: 1; font-size: 17px; padding: 10px 12px;
        border-radius: 10px; border: 1px solid #3a3f4e;
        background: #1d1d24; color: #cdd2dc;
      }
      .send {
        font-size: 17px; padding: 10px 14px; border-radius: 10px;
        border: 1px solid #3a3f4e; background: #1d1d24;
        color: #cdd2dc; cursor: pointer;
      }
```

- [ ] **Step 3: Build**: `npm --prefix web run build 2>&1 | tail -2` → built.
- [ ] **Step 4: Commit** `feat(webmic-web): отправка текста, файл-пикер и Ctrl+V картинок`.

---

### Task 5: E2E, живая проверка, README, пуш

**Files:**
- Modify: `src/webmic.rs` (e2e-тест), `README.md`

- [ ] **Step 1: e2e** — в `e2e_speech_to_finals` после проверки 403 добавить:

```rust
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let tmp_png = std::env::temp_dir().join("hw-e2e.png");
        std::fs::write(&tmp_png, &png).unwrap();
        let msg_code = curl_text(&["-sk", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST", "--data-binary", "привет", "-H", "Content-Type: text/plain", &format!("https://127.0.0.1:8787/api/msg?t={token}")]);
        assert_eq!(msg_code, "200");
        let img_code = curl_text(&["-sk", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST", "--data-binary", &format!("@{}", tmp_png.display()), "-H", "Content-Type: image/png", &format!("https://127.0.0.1:8787/api/img?t={token}")]);
        assert_eq!(img_code, "200");
        let bad = curl_text(&["-sk", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST", "--data-binary", "мусор", "-H", "Content-Type: image/png", &format!("https://127.0.0.1:8787/api/img?t={token}")]);
        assert_eq!(bad, "415");
        assert_eq!(_wm.shared().lock().unwrap().posts.len(), 2);
        let _ = std::fs::remove_file(&tmp_png);
```

- [ ] **Step 2: Прогон + сборка**: `cargo test 2>&1 | tail -3`; `cargo build --release`.
- [ ] **Step 3: Живая проверка** (перезапуск виджета setsid'ом с `HEALTH_WEBMIC=1 HEALTH_WEBMIC_OPEN=1`, т.к. токен у пользователя пока выключен):

```bash
setsid env HEALTH_AUTO_HIDE=0 HEALTH_WEBMIC=1 HEALTH_WEBMIC_OPEN=1 ./target/release/health-widget >/dev/null 2>&1 &
sleep 5
curl -sk -o /dev/null -w 'msg:%{http_code}\n' -X POST --data-binary 'тест' -H 'Content-Type: text/plain' https://localhost:8787/api/msg
curl -sk -o /dev/null -w 'img-bad:%{http_code}\n' -X POST --data-binary 'мусор' -H 'Content-Type: image/png' https://localhost:8787/api/img
```

Expected: `msg:200`, `img-bad:415`; текст «тест» виден в секции «📥 Присланное» окна hw-web.

- [ ] **Step 4: README** — в разделе «Веб-микрофон (🌐)» после абзаца про доступ добавить абзац: со страницы можно отправить текст (поле + Enter) и картинки (📎 или Ctrl+V-скриншот, png/jpeg/webp до 8 МБ); они появляются в нижней секции окна `hw-web` (последние 30, только просмотр), окно ресайзится за угол и запоминает размер.

- [ ] **Step 5: Финал**: `cargo test`, commit `test(webmic)+docs: e2e текст/картинки, README про ленту hw-web`, `git push origin master`.
