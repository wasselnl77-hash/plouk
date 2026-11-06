#!/usr/bin/env bash
# Helpers for the draft-first release flow (see .github/workflows/release.yml).
#
#   release.sh create            find or create the DRAFT release for the tag; prints its id
#   release.sh upload FILE...    add files to that draft (replacing same-named leftovers)
#   release.sh download DIR      download every file of that draft into DIR
#   release.sh publish           make the draft public and mark it the latest release
#
# Needs GH_TOKEN, GH_REPO and GITHUB_REF_NAME; everything but `create` also needs RELEASE_ID.
#
# A draft release is invisible to `releases/tags/<tag>` and to public download URLs, so nothing
# here looks it up by tag name (except `create`, which scans the full list) and nothing
# downloads from a public URL: files are added by release id and fetched through the API.
set -euo pipefail

: "${GH_TOKEN:?}" "${GH_REPO:?}" "${GITHUB_REF_NAME:?}"
TAG="$GITHUB_REF_NAME"
API="repos/$GH_REPO"

# Each line: "<id> <draft?>" for releases carrying this tag. Authenticated, so drafts are included.
releases_for_tag() {
  gh api --paginate "$API/releases?per_page=100" --jq ".[] | select(.tag_name == \"$TAG\") | \"\(.id) \(.draft)\""
}

create() {
  local found id draft
  found=$(releases_for_tag | sed -n 1p)
  if [ -n "$found" ]; then
    read -r id draft <<<"$found"
    if [ "$draft" != "true" ]; then
      echo "::error::Release $TAG is already public (id $id). Refusing to add to it." >&2
      exit 1
    fi
    echo "Reusing the existing draft release $id" >&2
  else
    id=$(gh api -X POST "$API/releases" -f tag_name="$TAG" -f name="$TAG" -F draft=true -F prerelease=false --jq .id)
    echo "Created draft release $id" >&2
  fi
  echo "$id"
}

need_id() {
  : "${RELEASE_ID:?RELEASE_ID is missing: the draft was not created}"
}

# Delete same-named assets (a half-finished earlier attempt), then upload.
put_asset() {
  local file=$1 name aid
  name=$(basename "$file")
  for aid in $(gh api --paginate "$API/releases/$RELEASE_ID/assets?per_page=100" --jq ".[] | select(.name == \"$name\") | .id"); do
    gh api -X DELETE "$API/releases/assets/$aid" > /dev/null
  done
  curl --fail --silent --show-error -X POST \
    -H "Authorization: Bearer $GH_TOKEN" \
    -H "Accept: application/vnd.github+json" \
    -H "Content-Type: application/octet-stream" \
    --data-binary @"$file" \
    "https://uploads.github.com/$API/releases/$RELEASE_ID/assets?name=$name" > /dev/null
}

upload() {
  need_id
  [ "$#" -gt 0 ] || { echo "::error::Nothing to upload" >&2; exit 1; }
  local file try ok
  for file in "$@"; do
    [ -f "$file" ] || { echo "::error::$file does not exist" >&2; exit 1; }
    ok=0
    for try in 1 2 3; do
      if put_asset "$file"; then ok=1; break; fi
      echo "Upload of $file failed (attempt $try)" >&2
      sleep $((try * 5))
    done
    [ "$ok" = 1 ] || { echo "::error::Could not upload $file" >&2; exit 1; }
    echo "Uploaded $(basename "$file")"
  done
}

download() {
  need_id
  local dir=${1:?directory} id name
  mkdir -p "$dir"
  gh api --paginate "$API/releases/$RELEASE_ID/assets?per_page=100" --jq '.[] | "\(.id) \(.name)"' | while read -r id name; do
    # The API answers with a redirect to the real file when asked for the raw bytes.
    gh api -H "Accept: application/octet-stream" "$API/releases/assets/$id" > "$dir/$name"
    echo "Downloaded $name"
  done
}

publish() {
  need_id
  gh api -X PATCH "$API/releases/$RELEASE_ID" -F draft=false -f make_latest=true --jq '"Published " + .html_url'
}

cmd=${1:-}
shift || true
case "$cmd" in
  create) create ;;
  upload) upload "$@" ;;
  download) download "$@" ;;
  publish) publish ;;
  *) echo "usage: release.sh create | upload FILE... | download DIR | publish" >&2; exit 2 ;;
esac
