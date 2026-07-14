import { useEffect, useRef, useState } from 'react'

const CHUNK_MS = 250

export default function App() {
  const [running, setRunning] = useState(false)
  const [finals, setFinals] = useState([])
  const [partial, setPartial] = useState('')
  const [stt, setStt] = useState(false)
  const [err, setErr] = useState('')
  const session = useRef(null)
  const feed = useRef(null)

  useEffect(() => {
    feed.current?.scrollTo(0, feed.current.scrollHeight)
  }, [finals, partial])

  useEffect(() => stop, [])

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
          const r = await fetch(`api/audio?rate=${ctx.sampleRate}&seq=${seq++}`, {
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
      <button className={running ? 'talk on' : 'talk'} onClick={running ? stop : start}>
        {running ? '⏹ Стоп' : '🎤 Говорить'}
      </button>
      <div className="feed" ref={feed}>
        {finals.length === 0 && !partial && (
          <div className="hint">жми «Говорить» и говори — текст появится тут и в виджете</div>
        )}
        {finals.map((t, i) => (
          <div key={i}>{t}</div>
        ))}
        {partial && <div className="partial">{partial}</div>}
      </div>
    </div>
  )
}
