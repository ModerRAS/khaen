#!/bin/sh
# TSPI PTP shared shell helpers (sourced by ptp-master-start / ptp-phc2sys-start).
# Debian 12 /bin/sh is dash: keep this POSIX, no bashisms.
# shellcheck shell=sh

ptp_log() { printf 'tspi-ptp: %s\n' "$*" >&2; }

# resolve_ptp_iface: echo the interface to serve PTP on.
# Honors TSPI_PTP_IFACE; "auto"/empty selects the interface of the IPv4 default route.
resolve_ptp_iface() {
    _iface=${TSPI_PTP_IFACE:-auto}
    case "$_iface" in
        ''|auto|AUTO)
            _iface=$(ip -4 route show default 2>/dev/null | awk '{for (i = 1; i <= NF; i++) if ($i == "dev") { print $(i + 1); exit }}')
            ;;
    esac
    if [ -z "$_iface" ]; then
        ptp_log "ERROR: cannot auto-detect interface (no IPv4 default route); set TSPI_PTP_IFACE"
        return 1
    fi
    if [ ! -e "/sys/class/net/$_iface" ]; then
        ptp_log "ERROR: interface not found: $_iface"
        return 1
    fi
    printf '%s\n' "$_iface"
}

# detect_ptp_phc <iface>: echo the PHC device (e.g. /dev/ptp0) when the interface can do
# hardware timestamping; return 1 when it cannot.
# Order: explicit TSPI_PTP_PHC override -> ethtool -T capabilities -> sysfs device/ptp.
detect_ptp_phc() {
    _iface=$1
    if [ -n "${TSPI_PTP_PHC:-}" ]; then
        if [ -e "${TSPI_PTP_PHC}" ]; then
            printf '%s\n' "${TSPI_PTP_PHC}"
            return 0
        fi
        ptp_log "ERROR: TSPI_PTP_PHC=${TSPI_PTP_PHC} does not exist"
        return 1
    fi
    _hw=0
    _et=''
    if command -v ethtool >/dev/null 2>&1; then
        _et=$(ethtool -T "$_iface" 2>/dev/null || true)
        case "$_et" in
            *"hardware-transmit"*|*"hardware-receive"*) _hw=1 ;;
        esac
        _idx=$(printf '%s\n' "$_et" | sed -n 's/^PTP Hardware Clock:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)
        if [ "$_hw" = "1" ] && [ -n "$_idx" ] && [ -e "/dev/ptp$_idx" ]; then
            printf '/dev/ptp%s\n' "$_idx"
            return 0
        fi
    fi
    for _d in /sys/class/net/"$_iface"/device/ptp/ptp*; do
        [ -e "$_d" ] || continue
        _n=$(basename "$_d")
        if [ -e "/dev/$_n" ]; then
            printf '/dev/%s\n' "$_n"
            return 0
        fi
    done
    if [ "$_hw" = "1" ]; then
        ptp_log "WARN: $_iface advertises hardware timestamping but no /dev/ptpN could be resolved"
    fi
    return 1
}
