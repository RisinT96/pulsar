#!/bin/sh
# Optional Mini 2 setup. Backups stay on the station; no credentials go in the repository.
set -eu
D=/data/pulsar
Q=/system/vendor/quasar
FLAG=/data/quasar/disable_stock_launcher
BACK=/data/quasar/run.sh.before-disable-stock
DNS=/data/unbound.conf.head
DNSBACK=/data/unbound.conf.head.before-pulsar
guard='[ ! -f /data/quasar/disable_stock_launcher ] || exit 0'

case ${1:-} in
  restore)
    [ -f "$DNSBACK" ] || { echo 'DNS backup missing' >&2; exit 1; }
    cp -p "$DNSBACK" "$DNS.new" && mv "$DNS.new" "$DNS"
    cp "$DNS" /etc/unbound/unbound.conf
    /etc/init.d/S70unbound restart
    rm -f "$FLAG"
    echo 'DNS restored and launcher guard disabled; q start can start the stock stack.'
    exit 0 ;;
  install) ;;
  *) echo 'usage: independent.sh install|restore' >&2; exit 1 ;;
esac

[ -f "$D/enabled" ] && [ -x "$D/pulsar" ] || { echo 'pulsar is not enabled' >&2; exit 1; }
[ -f "$DNS" ] && [ -f "$Q/run.sh" ] || { echo 'stock configuration missing' >&2; exit 1; }
if [ ! -f "$BACK" ]; then
  grep -Fq "$guard" "$Q/run.sh" && { echo 'guard present but original backup missing' >&2; exit 1; }
  cp -p "$Q/run.sh" "$BACK"
  cmp -s "$Q/run.sh" "$BACK" || { echo 'launcher backup verification failed' >&2; exit 1; }
fi
[ -f "$DNSBACK" ] || cp -p "$DNS" "$DNSBACK"
if ! grep -Fq "$guard" "$Q/run.sh"; then
  cmp -s "$BACK" "$Q/run.sh" || { echo 'launcher differs from backup; refusing to overwrite' >&2; exit 1; }
  { head -n 1 "$BACK"; printf '%s\n' '# Optional stock launcher guard.' "$guard"; tail -n +2 "$BACK"; } > "$D/run.sh.guarded"
  sh -n "$D/run.sh.guarded"
  was_ro=0
  awk '$2 == "/" && $4 ~ /(^|,)ro(,|$)/ { found=1 } END { exit !found }' /proc/mounts && was_ro=1
  if [ "$was_ro" = 1 ]; then mount -o remount,rw /; fi
  trap '[ "$was_ro" != 1 ] || mount -o remount,ro /' EXIT HUP INT TERM
  cp -p "$Q/run.sh" "$Q/run.sh.new"
  cat "$D/run.sh.guarded" > "$Q/run.sh.new"
  sh -n "$Q/run.sh.new"
  mv "$Q/run.sh.new" "$Q/run.sh"
  sync
  if [ "$was_ro" = 1 ]; then mount -o remount,ro /; fi
  trap - EXIT HUP INT TERM
fi
cat > "$D/unbound.conf.head.new" <<'EOF'
server:
    msg-cache-size: 2m
    rrset-cache-size: 2m
    username: "nobody"
forward-zone:
    name: "."
    forward-addr: "1.1.1.1"
    forward-addr: "9.9.9.9"
EOF
unbound-checkconf "$D/unbound.conf.head.new"
cp "$D/unbound.conf.head.new" "$DNS.new" && mv "$DNS.new" "$DNS"
cp "$DNS" /etc/unbound/unbound.conf
if ! /etc/init.d/S70unbound restart; then
  cp -p "$DNSBACK" "$DNS"
  cp "$DNS" /etc/unbound/unbound.conf
  /etc/init.d/S70unbound restart || true
  echo 'DNS restart failed; restored backup' >&2
  exit 1
fi
cp "$0" "$D/independent.sh.new" && chmod 755 "$D/independent.sh.new" && mv "$D/independent.sh.new" "$D/independent.sh"
: > "$FLAG"
sync
echo 'Launcher blocked and public DNS configured. DHCP may add DNS servers; firmware NTP is unchanged.'
echo 'To restore: /data/pulsar/independent.sh restore'
