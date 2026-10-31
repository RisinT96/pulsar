#!/bin/sh
# Optional Mini 2 setup. Backups stay on the station; no credentials go in the repository.
set -eu
D=/data/pulsar
Q=/system/vendor/quasar
FLAG=/data/quasar/disable_stock_launcher
BACK=/data/quasar/run.sh.before-disable-stock
DNS=/data/unbound.conf.head
DNSBACK=/data/unbound.conf.head.before-pulsar
CHRONY=/data/chrony.conf
CHRONYDEFAULT=/etc/default/chrony
CHRONYBACK=/data/chrony.conf.firmware-backup
guard='[ ! -f /data/quasar/disable_stock_launcher ] || exit 0'
chrony_args='CHRONY_ARGS="-f /data/chrony.conf"'

root_rw() {
  was_ro=0
  awk '$2 == "/" && $4 ~ /(^|,)ro(,|$)/ { found=1 } END { exit !found }' /proc/mounts && was_ro=1
  if [ "$was_ro" = 1 ]; then mount -o remount,rw /; fi
  trap '[ "$was_ro" != 1 ] || mount -o remount,ro /' EXIT HUP INT TERM
}
root_ro() {
  sync
  if [ "$was_ro" = 1 ]; then mount -o remount,ro /; fi
  trap - EXIT HUP INT TERM
}

case ${1:-} in
  restore)
    [ -f "$DNSBACK" ] || { echo 'DNS backup missing' >&2; exit 1; }
    if [ -e "$CHRONYDEFAULT" ]; then
      [ "$(cat "$CHRONYDEFAULT")" = "$chrony_args" ] || { echo 'Chrony override was changed; refusing to remove it' >&2; exit 1; }
      if grep -Eqi 'verity|(^|[[:space:]])dm=' /proc/cmdline; then echo 'Protected rootfs; refusing Chrony restore' >&2; exit 1; fi
      for mapper in /dev/mapper/*; do
        [ ! -e "$mapper" ] || [ "${mapper##*/}" = control ] || { echo 'Device mapper active; refusing Chrony restore' >&2; exit 1; }
      done
      root_rw
      rm -f "$CHRONYDEFAULT"
      root_ro
      /etc/init.d/S49chrony restart
    fi
    cp -p "$DNSBACK" "$DNS.new" && mv "$DNS.new" "$DNS"
    cp "$DNS" /etc/unbound/unbound.conf
    /etc/init.d/S70unbound restart
    rm -f "$FLAG"
    echo 'DNS and firmware Chrony restored; launcher guard disabled.'
    exit 0 ;;
  install) ;;
  *) echo 'usage: independent.sh install|restore' >&2; exit 1 ;;
esac

[ -f "$D/enabled" ] && [ -x "$D/pulsar" ] || { echo 'pulsar is not enabled' >&2; exit 1; }
[ -f "$DNS" ] && [ -f "$Q/run.sh" ] || { echo 'stock configuration missing' >&2; exit 1; }
[ -f /data/chrony_is_enabled ] && [ -f /etc/chrony.conf ] || { echo 'firmware Chrony is not enabled' >&2; exit 1; }
[ ! -e "$CHRONYDEFAULT" ] || [ "$(cat "$CHRONYDEFAULT")" = "$chrony_args" ] || { echo 'existing Chrony override differs; refusing to overwrite' >&2; exit 1; }
cat > "$D/chrony.conf.new" <<'EOF'
pool pool.ntp.org iburst
driftfile /var/lib/chrony/drift
makestep 1.0 3
rtcsync
EOF
[ ! -e "$CHRONY" ] || cmp -s "$D/chrony.conf.new" "$CHRONY" || { echo 'existing /data/chrony.conf differs; refusing to overwrite' >&2; exit 1; }
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
  root_rw
  cp -p "$Q/run.sh" "$Q/run.sh.new"
  cat "$D/run.sh.guarded" > "$Q/run.sh.new"
  sh -n "$Q/run.sh.new"
  mv "$Q/run.sh.new" "$Q/run.sh"
  root_ro
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
[ -f "$CHRONYBACK" ] || cp -p /etc/chrony.conf "$CHRONYBACK"
cp "$D/chrony.conf.new" "$CHRONY.new" && mv "$CHRONY.new" "$CHRONY"
new_chrony_hook=0
if [ ! -e "$CHRONYDEFAULT" ]; then
  root_rw
  mkdir -p /etc/default
  printf '%s\n' "$chrony_args" > /etc/default/.chrony.new
  chmod 644 /etc/default/.chrony.new
  mv /etc/default/.chrony.new "$CHRONYDEFAULT"
  root_ro
  new_chrony_hook=1
fi
if ! /etc/init.d/S49chrony restart || ! pidof chronyd >/dev/null 2>&1; then
  if [ "$new_chrony_hook" = 1 ]; then
    root_rw
    rm -f "$CHRONYDEFAULT"
    root_ro
    /etc/init.d/S49chrony start || true
  fi
  echo 'Chrony restart failed' >&2
  exit 1
fi
cp "$0" "$D/independent.sh.new" && chmod 755 "$D/independent.sh.new" && mv "$D/independent.sh.new" "$D/independent.sh" || { echo 'cannot install restore script' >&2; exit 1; }
: > "$FLAG"
sync
echo 'Launcher blocked, public DNS and NTP Pool configured. DHCP may add DNS servers.'
echo 'To restore: /data/pulsar/independent.sh restore'
