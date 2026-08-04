#!/usr/bin/env bash
set -euo pipefail

FN=alice-applied
SA=health-widget-alice
CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/health-widget"
CFG="$CFG_DIR/alice.json"
CLOUD="$CFG_DIR/alice-cloud.json"
SRC="$(cd "$(dirname "$0")/.." && pwd)/cloud/alice"

SKILL_ID=""
if [ "${1:-}" = "--skill-id" ]; then
  SKILL_ID="${2:?нужен id навыка}"
fi

if ! command -v yc >/dev/null; then
  curl -sSL https://storage.yandexcloud.net/yandexcloud-yc/install.sh | bash -s -- -a
  export PATH="$HOME/yandex-cloud/bin:$PATH"
fi

mkdir -p "$CFG_DIR"
[ -f "$CLOUD" ] || echo '{}' > "$CLOUD"

read_cloud() { jq -r --arg k "$1" '.[$k] // ""' "$CLOUD"; }
write_cloud() {
  tmp=$(mktemp)
  jq --arg k "$1" --arg v "$2" '.[$k] = $v' "$CLOUD" > "$tmp"
  mv "$tmp" "$CLOUD"
  chmod 600 "$CLOUD"
}

FOLDER=$(yc config get folder-id)

BUCKET=$(read_cloud bucket)
if [ -z "$BUCKET" ]; then
  BUCKET="health-widget-alice-$(head -c6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  write_cloud bucket "$BUCKET"
fi

TOKEN=$(read_cloud token)
if [ -z "$TOKEN" ]; then
  TOKEN=$(head -c24 /dev/urandom | od -An -tx1 | tr -d ' \n')
  write_cloud token "$TOKEN"
fi

[ -n "$SKILL_ID" ] && write_cloud skill_id "$SKILL_ID"
SKILL_ID=$(read_cloud skill_id)

if ! yc iam service-account get --name "$SA" >/dev/null 2>&1; then
  yc iam service-account create --name "$SA"
fi
SA_ID=$(yc iam service-account get --name "$SA" --format json | jq -r .id)

yc resource-manager folder add-access-binding "$FOLDER" \
  --role storage.editor --subject "serviceAccount:$SA_ID" >/dev/null 2>&1 || true

if ! yc storage bucket get --name "$BUCKET" >/dev/null 2>&1; then
  yc storage bucket create --name "$BUCKET"
fi

KEY_ID=$(read_cloud key_id)
SECRET=$(read_cloud key_secret)
if [ -z "$KEY_ID" ] || [ -z "$SECRET" ]; then
  KEY_JSON=$(yc iam access-key create --service-account-name "$SA" --format json)
  KEY_ID=$(echo "$KEY_JSON" | jq -r .access_key.key_id)
  SECRET=$(echo "$KEY_JSON" | jq -r .secret)
  write_cloud key_id "$KEY_ID"
  write_cloud key_secret "$SECRET"
fi

if ! yc serverless function get --name "$FN" >/dev/null 2>&1; then
  yc serverless function create --name "$FN"
fi

ENVS="ALICE_BUCKET=$BUCKET,ALICE_PUSH_TOKEN=$TOKEN,AWS_ACCESS_KEY_ID=$KEY_ID,AWS_SECRET_ACCESS_KEY=$SECRET"
[ -n "$SKILL_ID" ] && ENVS="$ENVS,ALICE_SKILL_ID=$SKILL_ID"

PKG=$(mktemp -d)
trap 'rm -rf "$PKG"' EXIT
cp "$SRC/handler.py" "$SRC/requirements.txt" "$PKG/"

yc serverless function version create \
  --function-name "$FN" \
  --runtime python312 \
  --entrypoint handler.handle \
  --memory 128m \
  --execution-timeout 5s \
  --source-path "$PKG" \
  --service-account-id "$SA_ID" \
  --environment "$ENVS" >/dev/null

yc serverless function allow-unauthenticated-invoke "$FN" >/dev/null 2>&1 || true

FN_ID=$(yc serverless function get --name "$FN" --format json | jq -r .id)
URL="https://functions.yandexcloud.net/$FN_ID"

jq -n --arg url "$URL" --arg token "$TOKEN" '{url: $url, token: $token}' > "$CFG"
chmod 600 "$CFG"

echo "функция: $FN_ID"
echo "url:     $URL"
echo "конфиг:  $CFG"
if [ -z "$SKILL_ID" ]; then
  echo
  echo "дальше: создай навык «автопилот» в Консоли Диалогов (backend = Cloud Function, id выше),"
  echo "затем прогони: scripts/alice-deploy.sh --skill-id <skill_id из Консоли>"
fi
