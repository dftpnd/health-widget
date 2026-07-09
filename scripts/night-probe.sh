#!/usr/bin/env bash
set -u

out_dir="${HOME}/.local/share/health-widget"
mkdir -p "$out_dir"
probe_log="${out_dir}/night-probe.log"
nvkms_log="${out_dir}/night-nvkms.log"
interval="${PROBE_INTERVAL:-20}"

journalctl -kf --since now -o short-iso -g "Failed to allocate NVKMS" >>"$nvkms_log" 2>/dev/null &
follower=$!
trap 'kill "$follower" 2>/dev/null' EXIT

printf 'старт: %s | интервал %ss | pid %s\n' "$(date -Is)" "$interval" "$$" >>"$probe_log"

while :; do
    ts="$(date -Is)"
    gpu="$(nvidia-smi --query-gpu=memory.used,memory.total,utilization.gpu,temperature.gpu --format=csv,noheader,nounits 2>/dev/null | tr -d ' ' | tr '\n' ';')"
    wpid="$(pgrep -x health-widget | head -1)"
    wrss="$(ps -o rss= -p "${wpid:-0}" 2>/dev/null | tr -d ' ')"
    wgpu="$(nvidia-smi --query-compute-apps=pid,used_memory --format=csv,noheader,nounits 2>/dev/null | awk -F', ' -v p="${wpid:-0}" '$1==p{print $2}')"
    mem="$(free -m | awk '/^Mem:/{print $3"/"$2"used;avail="$7}')"
    nvkms="$(wc -l <"$nvkms_log" 2>/dev/null | tr -d ' ')"
    printf '%s gpu=%s wpid=%s wrss_kb=%s wgpu_mib=%s ram_mb=%s nvkms=%s\n' \
        "$ts" "${gpu:-NA}" "${wpid:-none}" "${wrss:-NA}" "${wgpu:-0}" "$mem" "${nvkms:-0}" >>"$probe_log"
    sync -f "$probe_log" 2>/dev/null || true
    sleep "$interval"
done
