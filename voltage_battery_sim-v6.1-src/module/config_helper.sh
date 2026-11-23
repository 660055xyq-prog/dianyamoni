#!/system/bin/sh
MODDIR=${0%/*}
exec "$MODDIR/bin/vbat_sim" "$@"
