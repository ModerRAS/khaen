#!/usr/bin/env python3
"""Lab helper: step the BREN7P lab GM system clock to the PTP/TAI timescale.

The board design keeps the NIC PHC at TAI via `phc2sys -o 37` (hardware timestamping). The BREN7P lab
GM has no PHC, so for the TAI-semantics replay we set the GM system clock to "slave UTC + 37 s" using the
offset measured by the slave (pmc CURRENT_DATA_SET.offsetFromMaster == GM_time - slave_time):

    GM_new = GM_old - offset + 37 s

Run as root on the GM host. NOT a production tool: the board must use phc2sys/PHC, not a clock step.

Usage: tai-align-gm.py --offset-ns O [--utc-offset 37] [--dry-run]
"""
import argparse
import time


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--offset-ns", type=float,
                    help="slave measured offsetFromMaster in ns (GM - slave); target = now - offset + utc_offset")
    ap.add_argument("--delta-ns", type=float,
                    help="explicit clock step in ns (used for the fine re-alignment pass); overrides --offset-ns")
    ap.add_argument("--utc-offset", type=int, default=37, help="TAI-UTC offset seconds (default 37)")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    if args.delta_ns is not None:
        delta = int(round(args.delta_ns))
    elif args.offset_ns is not None:
        delta = int(round(args.utc_offset * 1_000_000_000 - args.offset_ns))
    else:
        ap.error("one of --offset-ns/--delta-ns is required")
    now = time.clock_gettime_ns(time.CLOCK_REALTIME)
    target = now + delta
    print(f"delta_requested_ns={delta} utc_offset={args.utc_offset}s")
    print(f"clock_before={now} delta_ns={delta} clock_target={target}")
    if args.dry_run:
        print("dry-run: no clock change")
        return 0
    time.clock_settime_ns(time.CLOCK_REALTIME, target)
    print(f"clock_after={time.clock_gettime_ns(time.CLOCK_REALTIME)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
