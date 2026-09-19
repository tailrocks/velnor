#!/usr/bin/env bash
set -euo pipefail

# External adversarial harness for the committed APT application-release
# discovery helper. It mutates only synthetic GitHub API responses and never
# edits the source tree.
SCRIPT="${SCRIPT:-/tmp/g2-apt-discovery-review/scripts/release-discovery.sh}"
OUT="${1:-$(pwd)/apt-discovery-adversarial}"
rm -rf -- "$OUT"
mkdir -p "$OUT"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum -- "$1" | awk '{print $1}'
  else shasum -a 256 -- "$1" | awk '{print $1}'; fi
}

make_case() {
  local case_name="$1"
  local work="$OUT/$case_name"
  local source="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  local manifest="$work/assets/10" parent amd_sha arm_sha amd_size arm_size record_sha
  local amd_name="velnor-runner-1.2.3-amd64.deb"
  local arm_name="velnor-runner-1.2.3-arm64.deb"
  mkdir -p "$work/bin" "$work/assets"

  cat > "$work/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
endpoint=$(printf '%s\n' "$*" | awk '{print $NF}')
case "$*" in
  *"releases?per_page=100") cat "$FAKE_ROOT/releases.json" ;;
  *"/releases/assets/"*)
    id=$(printf '%s\n' "$endpoint" | sed 's#^.*/##')
    cat "$FAKE_ROOT/assets/$id"
    ;;
  *"/git/ref/tags/"*)
    commit=$(jq -er '.target_commitish' "$FAKE_ROOT/release.json")
    if env | rg -q '^FAKE_REF_MISMATCH=1$'; then commit=ffffffffffffffffffffffffffffffffffffffff; fi
    jq -cn --arg commit "$commit" '{object:{type:"commit",sha:$commit}}'
    ;;
  *) echo "unexpected gh invocation: $*" >&2; exit 2 ;;
esac
SH
  chmod +x "$work/bin/gh"

  printf 'fixture-amd64\n' > "$work/assets/17"
  printf 'fixture-arm64\n' > "$work/assets/19"
  amd_sha="$(sha256_file "$work/assets/17")"
  arm_sha="$(sha256_file "$work/assets/19")"
  amd_size="$(wc -c < "$work/assets/17" | tr -d '[:space:]')"
  arm_size="$(wc -c < "$work/assets/19" | tr -d '[:space:]')"

  jq -S -n \
    --arg source "$source" --arg amd "$amd_sha" --arg arm "$arm_sha" \
    --argjson amd_size "$amd_size" --argjson arm_size "$arm_size" \
    --argjson homebrew "$([[ "$case_name" == homebrew-digest-tamper ]] && printf true || printf false)" \
    --arg case "$case_name" \
    ' {
      schema:"velnor.product-manifest/v1", product_id:"velnor", channel:"stable",
      version:"1.2.3", source_repository:"tailrocks/velnor",
      source_ref:"refs/tags/v1.2.3", source_commit:$source,
      release_tag:"v1.2.3", release_id:(if $case == "release-id-grammar" then "bad release id" else "release-77" end),
      artifacts:(
        [{name:"velnor-runner-1.2.3-amd64.deb",target:"x86_64-unknown-linux-gnu",kind:"apt-package",sha256:$amd,size:$amd_size},
         {name:"velnor-runner-1.2.3-arm64.deb",target:"aarch64-unknown-linux-gnu",kind:"apt-package",sha256:$arm,size:$arm_size}]
        + (if $homebrew then [{name:"velnorctl-1.2.3-aarch64-apple-darwin.tar.gz",target:"aarch64-apple-darwin",kind:"homebrew-archive",sha256:("c"*64),size:15}] else [] end)
      ),
      components:[
        {name:"velnor-runner",crate:"velnor-runner",version:"0.1.277",binary:"velnor-runner",targets:["x86_64-unknown-linux-gnu","aarch64-unknown-linux-gnu","aarch64-apple-darwin"]},
        {name:"velnorctl",crate:"velnorctl",version:"0.1.0",binary:(if $case == "binary-name-drift" then "evilctl" else "velnorctl" end),targets:["x86_64-unknown-linux-gnu","aarch64-unknown-linux-gnu","aarch64-apple-darwin"]},
        {name:"velnor-workflow",crate:"velnor-workflow",version:"0.1.0",binary:"velnor-workflow",targets:["x86_64-unknown-linux-gnu","aarch64-unknown-linux-gnu","aarch64-apple-darwin"]}
      ]
    }' > "$manifest"
  parent="$(sha256_file "$manifest")"
  printf '%s\n' "$parent" > "$work/assets/11"

  jq -S -n --arg parent "$parent" --arg source "$source" --arg amd "$amd_sha" --arg arm "$arm_sha" \
    --argjson census "$([[ "$case_name" == release-record-census ]] && printf false || printf true)" \
    '{schema:"velnor.release-record/v1",parent_manifest_sha256:$parent,
      build:{repository:"tailrocks/velnor",tag:"v1.2.3",commit:$source,crate_version:"1.2.3",debian_version:"1.2.3",manifest_version:1,manifest_sha256:("d"*64)},
      architectures:(if $census then [
        {arch:"amd64",target:"x86_64-unknown-linux-gnu",binary_sha256:("e"*64),deb_sha256:$amd,oci_platform_digest:("sha256:"+ ("a"*64))},
        {arch:"arm64",target:"aarch64-unknown-linux-gnu",binary_sha256:("f"*64),deb_sha256:$arm,oci_platform_digest:("sha256:"+ ("b"*64))}
      ] else [] end),
      oci_index_digest:("sha256:" + ("c"*64)),oci_image_ref:("ghcr.io/tailrocks/velnor/app@sha256:" + ("c"*64)),
      oci_labels:{version:"1.2.3",revision:$source,source:"https://github.com/tailrocks/velnor",manifest_sha256:("d"*64)},
      apt:{origin:"Velnor",suite:"stable",component:"main"}}' > "$work/assets/12"
  printf '%s\n' "$(sha256_file "$work/assets/12")" > "$work/assets/121"

  if [[ "$case_name" == release-manifest-assets-tamper ]]; then
    jq -S -n --arg parent "$parent" --arg source "$source" \
      '{schema:"velnor.package-release.v1",parent_manifest_sha256:$parent,source_repository:"tailrocks/velnor",source_ref:"refs/tags/v1.2.3",source_commit:$source,version:"1.2.3",assets:[{name:"wrong.deb",sha256:("e"*64)}]}' \
      > "$work/assets/13"
  else
    jq -S -n --arg parent "$parent" --arg source "$source" --arg amd "$amd_sha" --arg arm "$arm_sha" \
      '{schema:"velnor.package-release.v1",parent_manifest_sha256:$parent,source_repository:"tailrocks/velnor",source_ref:"refs/tags/v1.2.3",source_commit:$source,version:"1.2.3",assets:[{name:"velnor-runner-1.2.3-amd64.deb",sha256:$amd},{name:"velnor-runner-1.2.3-arm64.deb",sha256:$arm}]}' \
      > "$work/assets/13"
  fi
  jq -S -n --arg parent "$parent" --arg source "$source" \
    '{parent_manifest_sha256:$parent,source_sha:$source,version:1,crate_version:"0.1.277"}' > "$work/assets/14"
  record_sha="$(sha256_file "$work/assets/14")"
  jq --arg manifest_sha "$record_sha" '.build.manifest_sha256 = $manifest_sha | .oci_labels.manifest_sha256 = $manifest_sha' \
    "$work/assets/12" > "$work/assets/12.tmp"
  mv "$work/assets/12.tmp" "$work/assets/12"
  printf '%s\n' "$(sha256_file "$work/assets/12")" > "$work/assets/121"
  printf '%s\n' "$(sha256_file "$work/assets/14")" > "$work/assets/141"
  printf '%s  %s\n%s  %s\n' "$amd_sha" "$amd_name" "$arm_sha" "$arm_name" > "$work/assets/15"
  if [[ "$case_name" == sha256-sums-tamper ]]; then printf 'tampered\n' > "$work/assets/15"; fi
  printf '%s\n' "$amd_sha" > "$work/assets/16"
  printf '%s\n' "$arm_sha" > "$work/assets/18"

  local assets='[]' tag='v1.2.3' urlbase='https://github.com/tailrocks/velnor/releases/download/v1.2.3/'
  assets=$(jq -c --arg url "$urlbase" --arg amd "$amd_name" --arg arm "$arm_name" \
    --argjson amd_size "$amd_size" --argjson arm_size "$arm_size" '
    [
      {id:10,name:"product-manifest.json",size:1,state:"uploaded",browser_download_url:($url+"product-manifest.json")},
      {id:11,name:"product-manifest.json.sha256",size:1,state:"uploaded",browser_download_url:($url+"product-manifest.json.sha256")},
      {id:13,name:"release-manifest.json",size:1,state:"uploaded",browser_download_url:($url+"release-manifest.json")},
      {id:15,name:"SHA256SUMS",size:1,state:"uploaded",browser_download_url:($url+"SHA256SUMS")},
      {id:12,name:"release-record.json",size:1,state:"uploaded",browser_download_url:($url+"release-record.json")},
      {id:121,name:"release-record.json.sha256",size:1,state:"uploaded",browser_download_url:($url+"release-record.json.sha256")},
      {id:14,name:"manifest.json",size:1,state:"uploaded",browser_download_url:($url+"manifest.json")},
      {id:141,name:"manifest.json.sha256",size:1,state:"uploaded",browser_download_url:($url+"manifest.json.sha256")},
      {id:17,name:$amd,size:$amd_size,state:"uploaded",browser_download_url:($url+$amd)},
      {id:16,name:($amd+".sha256"),size:1,state:"uploaded",browser_download_url:($url+$amd+".sha256")},
      {id:19,name:$arm,size:$arm_size,state:"uploaded",browser_download_url:($url+$arm)},
      {id:18,name:($arm+".sha256"),size:1,state:"uploaded",browser_download_url:($url+$arm+".sha256")}
    ]' <<< "$assets")
  if [[ "$case_name" == homebrew-digest-tamper ]]; then
    printf 'tampered-homebrew\n' > "$work/assets/20"
    assets=$(jq -c --arg url "$urlbase" '. + [{id:20,name:"velnorctl-1.2.3-aarch64-apple-darwin.tar.gz",size:15,state:"uploaded",browser_download_url:($url+"velnorctl-1.2.3-aarch64-apple-darwin.tar.gz")}]' <<< "$assets")
  fi
  if [[ "$case_name" == asset-url-missing ]]; then
    assets=$(jq '(.[] | select(.name == "product-manifest.json")).browser_download_url = ""' <<< "$assets")
  fi
  jq -S -n --argjson assets "$assets" \
    '{id:77,tag_name:"v1.2.3",draft:false,prerelease:false,target_commitish:"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",html_url:"https://github.com/tailrocks/velnor/releases/tag/v1.2.3",published_at:"2026-09-20T00:00:00Z",assets:$assets}' \
    > "$work/release.json"
  if [[ "$case_name" == ambiguous-version ]]; then
    jq '.id = 78 | .html_url = "https://github.com/tailrocks/velnor/releases/tag/v1.2.3-duplicate"' "$work/release.json" > "$work/release-duplicate.json"
    jq -c -s . "$work/release.json" "$work/release-duplicate.json" > "$work/releases.json"
  else
    jq -c -s . "$work/release.json" > "$work/releases.json"
  fi
}

run_case() {
  local name="$1" expected="$2"
  local work="$OUT/$name" output="$OUT/$name.json"
  make_case "$name"
  set +e
  if [[ "$name" == source-ref-mismatch ]]; then
    FAKE_REF_MISMATCH=1 FAKE_ROOT="$work" PATH="$work/bin:$PATH" "$SCRIPT" --channel stable > "$output" 2> "$OUT/$name.stderr"
  else
    FAKE_ROOT="$work" PATH="$work/bin:$PATH" "$SCRIPT" --channel stable > "$output" 2> "$OUT/$name.stderr"
  fi
  local status=$?
  set -e
  printf '%s\texpected=%s\texit=%s\n' "$name" "$expected" "$status" | tee -a "$OUT/results.tsv"
  if [[ "$expected" == pass && "$status" -ne 0 ]] ||
     [[ "$expected" == fail && "$status" -eq 0 ]]; then
    echo "unexpected result for $name" >&2
    exit 1
  fi
}

: > "$OUT/results.tsv"
run_case baseline pass
run_case binary-name-drift fail
run_case sha256-sums-tamper fail
run_case release-manifest-assets-tamper fail
run_case release-record-census fail
run_case homebrew-digest-tamper fail
run_case asset-url-missing fail
run_case source-ref-mismatch fail
run_case ambiguous-version fail
run_case release-id-grammar fail
echo "fixtures written to $OUT"
