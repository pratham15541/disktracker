#!/bin/bash
set -e

PATH_TO_SCAN="${1:-.}"
OUTPUT_FILE="${2:-}"

echo "Building disktracker in release mode..."
cargo build --release

DISKTRACKER="./target/release/disktracker"

echo "Running cold scan (full traversal forced)..."
COLD_RESULT=$($DISKTRACKER scan "$PATH_TO_SCAN" --bench --cold)

echo "Running warm scan (with cached skip predicate)..."
WARM_RESULT=$($DISKTRACKER scan "$PATH_TO_SCAN" --bench)

# Extract fields from the JSON results
COLD_MS=$(echo "$COLD_RESULT" | grep -o '"wall_ms": [0-9]*' | grep -o '[0-9]*' || echo "0")
WARM_MS=$(echo "$WARM_RESULT" | grep -o '"wall_ms": [0-9]*' | grep -o '[0-9]*' || echo "0")
TOTAL_FILES=$(echo "$COLD_RESULT" | grep -o '"total_files": [0-9]*' | grep -o '[0-9]*' || echo "0")
TOTAL_DIRS=$(echo "$COLD_RESULT" | grep -o '"total_dirs": [0-9]*' | grep -o '[0-9]*' || echo "0")
TOTAL_BYTES=$(echo "$COLD_RESULT" | grep -o '"total_bytes": [0-9]*' | grep -o '[0-9]*' || echo "0")

if [ "$WARM_MS" -eq 0 ]; then WARM_MS=1; fi
SPEEDUP=$(echo "scale=2; $COLD_MS / $WARM_MS" | bc -l)

echo "--------------------------------------------------"
echo "Benchmark Summary for $PATH_TO_SCAN"
echo "--------------------------------------------------"
echo "Total Files:   $TOTAL_FILES"
echo "Total Dirs:    $TOTAL_DIRS"
echo "Total Bytes:   $TOTAL_BYTES bytes"
echo "Cold Scan:     ${COLD_MS}ms"
echo "Warm Scan:     ${WARM_MS}ms"
echo "Speedup Ratio: ${SPEEDUP}x"
echo "--------------------------------------------------"

if [ -n "$OUTPUT_FILE" ]; then
  cat <<EOF > "$OUTPUT_FILE"
{
  "path": "$PATH_TO_SCAN",
  "total_files": $TOTAL_FILES,
  "total_dirs": $TOTAL_DIRS,
  "total_bytes": $TOTAL_BYTES,
  "cold_ms": $COLD_MS,
  "warm_ms": $WARM_MS,
  "speedup": $SPEEDUP
}
EOF
  echo "Saved benchmark results to $OUTPUT_FILE"
fi
