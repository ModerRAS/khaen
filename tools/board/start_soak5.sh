#!/bin/bash
set -e
D=/var/lib/khaen-soak
STAMP=$(date +%Y%m%d-%H%M%S)
mkdir -p "$D/prev-$STAMP"
for f in status.csv soak.log camilla.log aplay.log camilla.yml; do
  [ -e "$D/$f" ] && mv "$D/$f" "$D/prev-$STAMP/"
done
modprobe snd-aloop
cat > /tmp/camilla-fifo80 <<'EOF'
#!/bin/sh
exec chrt -f 80 taskset -c 4-7 /usr/local/bin/camilladsp "$@"
EOF
chmod +x /tmp/camilla-fifo80
echo "== cards =="
cat /proc/asound/cards
echo "== web =="
curl -s -o /dev/null -m 3 -w '%{http_code}\n' "http://127.0.0.1:${KHAEN_WEB_PORT:-80}/healthz" || true
echo "== start =="
systemd-run --unit=khaen-soak5 --collect --property=Restart=no /var/lib/khaen-soak/soak12.sh 5
sleep 3
systemctl is-active khaen-soak5.service
tail -10 /var/lib/khaen-soak/soak.log