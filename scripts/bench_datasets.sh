#!/bin/bash
set -e

# Ensure we are running from the workspace root
cd "$(dirname "$0")/.."

echo "========================================================================================"
echo "                   Building DiskTracker in Release Mode..."
echo "========================================================================================"
cargo build --release

DISKTRACKER="./target/release/disktracker"
BENCH_SCRIPT="./scripts/bench.sh"
BENCH_OUT_DIR="./target/benchmarks"
mkdir -p "$BENCH_OUT_DIR"

# Define datasets to benchmark
DATASETS=()
DATASET_NAMES=()

# 1. Workspace (git repo)
DATASETS+=(".")
DATASET_NAMES+=("Workspace (Git)")

# 2. node_modules (npm forest)
if [ -d "node_modules" ]; then
    DATASETS+=("node_modules")
    DATASET_NAMES+=("node_modules")
fi

# 3. /usr directory (system files)
if [ -d "/usr" ] && [ -r "/usr" ]; then
    DATASETS+=("/usr")
    DATASET_NAMES+=("/usr (System)")
fi

# 4. Home directory or parent (user files)
if [ -n "$HOME" ] && [ -d "$HOME" ] && [ -r "$HOME" ]; then
    DATASETS+=("$HOME")
    DATASET_NAMES+=("Home Directory")
fi

echo "========================================================================================"
echo "                   Starting DiskTracker Dataset Benchmarks"
echo "========================================================================================"
echo "Detected ${#DATASETS[@]} benchmark datasets."
echo ""

# Run benchmarks
for i in "${!DATASETS[@]}"; do
    path="${DATASETS[$i]}"
    name="${DATASET_NAMES[$i]}"
    safe_name=$(echo "$name" | tr -cd '[:alnum:]_')
    json_out="$BENCH_OUT_DIR/${safe_name}.json"
    
    echo "--> Running benchmark on: $name ($path)"
    bash "$BENCH_SCRIPT" "$path" "$json_out"
    echo ""
done

# Print beautifully formatted summary table
echo "========================================================================================"
echo "                      DISKTRACKER PHASE A DATASET BENCHMARKS"
echo "========================================================================================"
printf "%-22s | %-10s | %-8s | %-12s | %-9s | %-9s | %-8s\n" "Dataset Name" "Files" "Dirs" "Size (MB)" "Cold (ms)" "Warm (ms)" "Speedup"
echo "-----------------------+------------+----------+--------------+-----------+-----------+---------"

for i in "${!DATASETS[@]}"; do
    name="${DATASET_NAMES[$i]}"
    safe_name=$(echo "$name" | tr -cd '[:alnum:]_')
    json_out="$BENCH_OUT_DIR/${safe_name}.json"
    
    if [ -f "$json_out" ]; then
        # Parse fields from json
        total_files=$(grep -o '"total_files": [0-9]*' "$json_out" | head -n1 | grep -o '[0-9]*')
        total_dirs=$(grep -o '"total_dirs": [0-9]*' "$json_out" | head -n1 | grep -o '[0-9]*')
        total_bytes=$(grep -o '"total_bytes": [0-9]*' "$json_out" | head -n1 | grep -o '[0-9]*')
        cold_ms=$(grep -o '"cold_ms": [0-9]*' "$json_out" | head -n1 | grep -o '[0-9]*')
        warm_ms=$(grep -o '"warm_ms": [0-9]*' "$json_out" | head -n1 | grep -o '[0-9]*')
        speedup=$(grep -o '"speedup": [0-9.]*' "$json_out" | head -n1 | grep -o '[0-9.]*')
        
        # Format size in MB (with 2 decimal places)
        size_mb=$(echo "scale=2; $total_bytes / 1048576" | bc -l)
        
        printf "%-22s | %-10s | %-8s | %-12s | %-9s | %-9s | %-8sx\n" \
            "$name" "$total_files" "$total_dirs" "$size_mb" "$cold_ms" "$warm_ms" "$speedup"
    fi
done
echo "========================================================================================"
echo "All benchmark runs completed successfully."
