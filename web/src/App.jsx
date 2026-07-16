import { useEffect, useRef, useState } from 'react'

const CHUNK_MS = 250
const TOKEN = new URLSearchParams(location.search).get('t') || ''

export default function App() {
  const [running, setRunning] = useState(false)
  const [finals, setFinals] = useState([])
  const [partial, setPartial] = useState('')
  const [stt, setStt] = useState(false)
  const [err, setErr] = useState('')
  const [msg, setMsg] = useState('')
  const [sent, setSent] = useState([])
  const [saidStash, setSaidStash] = useState(null)
  const [sentStash, setSentStash] = useState(null)
  const [zoom, setZoom] = useState({ on: false, text: '', partial: '' })
  const session = useRef(null)
  const feed = useRef(null)
  const zoomFeed = useRef(null)
  const sentFeed = useRef(null)
  const fileRef = useRef(null)

  useEffect(() => {
    feed.current?.scrollTo(0, feed.current.scrollHeight)
  }, [finals, partial])

  useEffect(() => {
    zoomFeed.current?.scrollTo(0, zoomFeed.current.scrollHeight)
  }, [zoom])

  useEffect(() => {
    let ws
    let timer
    let closed = false
    const connect = () => {
      ws = new WebSocket(`wss://${location.host}/api/zoom?t=${TOKEN}`)
      ws.onmessage = (e) => {
        let m
        try {
          m = JSON.parse(e.data)
        } catch {
          return
        }
        setZoom((z) => ({
          on: m.on !== undefined ? !!m.on : z.on,
          text: m.full !== undefined ? m.full : m.add ? z.text + m.add : z.text,
          partial: m.partial !== undefined ? m.partial : z.partial,
        }))
      }
      ws.onclose = () => {
        if (!closed) timer = setTimeout(connect, 2000)
      }
    }
    connect()
    return () => {
      closed = true
      clearTimeout(timer)
      ws?.close()
    }
  }, [])

  useEffect(() => {
    sentFeed.current?.scrollTo(0, sentFeed.current.scrollHeight)
  }, [sent])

  useEffect(() => stop, [])

  async function post(path, body, type) {
    const sep = path.includes('?') ? '&' : '?'
    const r = await fetch(`${path}${sep}t=${TOKEN}`, {
      method: 'POST',
      headers: type ? { 'Content-Type': type } : {},
      body,
    })
    if (!r.ok) throw new Error(String(r.status))
  }

  async function clearSaid() {
    if (finals.length === 0) return
    setSaidStash(finals)
    setFinals([])
    try {
      await post('api/clear?what=said', '')
      setErr('')
    } catch {
      setErr('очистка не дошла до виджета')
    }
  }

  async function undoSaid() {
    if (!saidStash) return
    setFinals((f) => [...saidStash, ...f])
    setSaidStash(null)
    try {
      await post('api/undo?what=said', '')
      setErr('')
    } catch {
      setErr('возврат не дошёл до виджета')
    }
  }

  async function clearSent() {
    if (sent.length === 0) return
    setSentStash(sent)
    setSent([])
    try {
      await post('api/clear?what=sent', '')
      setErr('')
    } catch {
      setErr('очистка не дошла до виджета')
    }
  }

  async function undoSent() {
    if (!sentStash) return
    setSent((s) => [...sentStash, ...s])
    setSentStash(null)
    try {
      await post('api/undo?what=sent', '')
      setErr('')
    } catch {
      setErr('возврат не дошёл до виджета')
    }
  }

  async function sendText() {
    const text = msg.trim()
    if (!text) return
    try {
      await post('api/msg', text, 'text/plain; charset=utf-8')
      setSent((s) => [...s, { kind: 'text', text }])
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
      setSent((s) => [...s, { kind: 'img', url: URL.createObjectURL(file), label }])
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

  async function start() {
    setErr('')
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true },
      })
      const ctx = new AudioContext()
      await ctx.audioWorklet.addModule('worklet.js')
      const src = ctx.createMediaStreamSource(stream)
      const tap = new AudioWorkletNode(ctx, 'mic-tap')
      src.connect(tap)
      let buf = []
      tap.port.onmessage = (e) => buf.push(e.data)
      let seq = 0
      let inFlight = false
      const timer = setInterval(async () => {
        if (inFlight || buf.length === 0) return
        const chunks = buf
        buf = []
        const pcm = new Float32Array(chunks.reduce((n, c) => n + c.length, 0))
        let off = 0
        for (const c of chunks) {
          pcm.set(c, off)
          off += c.length
        }
        inFlight = true
        try {
          const r = await fetch(`api/audio?rate=${ctx.sampleRate}&seq=${seq++}&t=${TOKEN}`, {
            method: 'POST',
            body: pcm.buffer,
          })
          const j = await r.json()
          if (j.finals?.length) setFinals((f) => [...f, ...j.finals])
          setPartial(j.partial || '')
          setStt(!!j.stt)
          setErr('')
        } catch {
          setErr('связь с виджетом потеряна')
        } finally {
          inFlight = false
        }
      }, CHUNK_MS)
      session.current = { stream, ctx, timer }
      setRunning(true)
    } catch (e) {
      setErr('микрофон недоступен: ' + e.message)
    }
  }

  function stop() {
    const s = session.current
    if (s) {
      clearInterval(s.timer)
      s.stream.getTracks().forEach((t) => t.stop())
      s.ctx.close()
    }
    session.current = null
    setRunning(false)
    setPartial('')
    setStt(false)
  }

  const status = !running
    ? 'микрофон выключен'
    : stt
      ? 'распознаю'
      : 'жду whisper…'

  return (
    <div className="app">
      <div className="top">
        <span className="title">🌐 Веб-микрофон</span>
        <span className="status">{status}</span>
        {err && <span className="err">✖ {err}</span>}
      </div>
      <div className="cols">
        <div className="col">
          <button className={running ? 'talk on' : 'talk'} onClick={running ? stop : start}>
            {running ? '⏹ Стоп' : '🎤 Говорить'}
          </button>
          <div className="feedbar">
            <span className="zoomhdr">🗣 Речь</span>
            <span className="gap" />
            <button className="mini" onClick={clearSaid} disabled={finals.length === 0} title="очистить сказанное">
              🗑
            </button>
            <button className="mini" onClick={undoSaid} disabled={!saidStash} title="вернуть последнюю очистку">
              ↩
            </button>
          </div>
          <div className="feed" ref={feed}>
            {finals.length === 0 && !partial && (
              <div className="hint">жми «Говорить» и говори — текст появится тут и в виджете</div>
            )}
            {finals.map((t, i) => (
              <div key={i}>{t}</div>
            ))}
            {partial && <div className="partial">{partial}</div>}
          </div>
          <div className="zoomhdr">🔊 Телемост</div>
          <div className="feed zoom" ref={zoomFeed}>
            {!zoom.on && !zoom.text && (
              <div className="hint">захват телемоста выключен — включи 🔊 в виджете</div>
            )}
            {zoom.on && !zoom.text && !zoom.partial && (
              <div className="hint">жду речь из телемоста…</div>
            )}
            {zoom.text}
            {zoom.partial && <span className="partial"> {zoom.partial}</span>}
          </div>
        </div>
        <div className="col">
          <div className="feedbar">
            <span className="zoomhdr">✉ Отправленное</span>
            <span className="gap" />
            <button className="mini" onClick={clearSent} disabled={sent.length === 0} title="очистить отправленное">
              🗑
            </button>
            <button className="mini" onClick={undoSent} disabled={!sentStash} title="вернуть последнюю очистку">
              ↩
            </button>
          </div>
          <div className="feed sent" ref={sentFeed}>
            {sent.length === 0 && (
              <div className="hint">текст и картинки для виджета — сюда: поле ниже, 📎 или Ctrl+V</div>
            )}
            {sent.map((it, i) =>
              it.kind === 'text' ? (
                <div key={i}>→ {it.text}</div>
              ) : (
                <img key={i} className="thumb" src={it.url} alt={it.label} title={it.label} />
              )
            )}
          </div>
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
        </div>
      </div>
    </div>
  )
}
