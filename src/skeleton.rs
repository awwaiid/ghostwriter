//! Zhang-Suen thinning and skeleton-to-polyline tracing.
//!
//! Converts a filled binary image to 1-pixel-wide skeleton paths.
/// Apply Zhang-Suen thinning algorithm in-place.
/// `grid[row][col]` = true means ink pixel.
pub fn thin_zhang_suen(grid: &mut [Vec<bool>]) {
    let rows = grid.len();
    if rows < 3 {
        return;
    }
    let cols = grid[0].len();
    if cols < 3 {
        return;
    }

    loop {
        let mut changed = false;

        for pass in 0..2u8 {
            let mut to_remove = Vec::new();

            for r in 1..rows - 1 {
                for c in 1..cols - 1 {
                    if !grid[r][c] {
                        continue;
                    }

                    // 8 neighbors clockwise from top: P2..P9
                    let p = [
                        grid[r - 1][c],     // P2 top
                        grid[r - 1][c + 1], // P3 top-right
                        grid[r][c + 1],     // P4 right
                        grid[r + 1][c + 1], // P5 bottom-right
                        grid[r + 1][c],     // P6 bottom
                        grid[r + 1][c - 1], // P7 bottom-left
                        grid[r][c - 1],     // P8 left
                        grid[r - 1][c - 1], // P9 top-left
                    ];

                    // N: number of non-zero neighbors
                    let n: usize = p.iter().filter(|&&v| v).count();
                    if !(2..=6).contains(&n) {
                        continue;
                    }

                    // S: number of 0→1 transitions in the circular sequence
                    let s = (0..8usize).filter(|&i| !p[i] && p[(i + 1) % 8]).count();
                    if s != 1 {
                        continue;
                    }

                    let (p2, p4, p6, p8) = (p[0], p[2], p[4], p[6]);
                    // Written to match the Zhang-Suen paper's conditions
                    // (P2*P4*P6 = 0 and P4*P6*P8 = 0) rather than clippy's minimized form.
                    #[allow(clippy::nonminimal_bool)]
                    let remove = if pass == 0 {
                        (!p2 || !p4 || !p6) && (!p4 || !p6 || !p8)
                    } else {
                        (!p2 || !p4 || !p8) && (!p2 || !p6 || !p8)
                    };

                    if remove {
                        to_remove.push((r, c));
                    }
                }
            }

            for (r, c) in to_remove {
                grid[r][c] = false;
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
}

/// Get 8-connected neighbors of pixel (r, c) that are set in `grid`.
fn on_neighbors(grid: &[Vec<bool>], r: usize, c: usize) -> Vec<(usize, usize)> {
    let rows = grid.len();
    let cols = grid[0].len();
    let mut out = Vec::with_capacity(8);
    for dr in -1i32..=1 {
        for dc in -1i32..=1 {
            if dr == 0 && dc == 0 {
                continue;
            }
            let nr = r as i32 + dr;
            let nc = c as i32 + dc;
            if nr >= 0 && nc >= 0 {
                let (nr, nc) = (nr as usize, nc as usize);
                if nr < rows && nc < cols && grid[nr][nc] {
                    out.push((nr, nc));
                }
            }
        }
    }
    out
}

/// Smooth a path by computing a rolling average over `window` points.
/// Keeps endpoints fixed.
pub fn smooth_path(path: &[(f32, f32)], window: usize) -> Vec<(f32, f32)> {
    if path.len() <= 2 || window < 2 {
        return path.to_vec();
    }
    let half = window / 2;
    let n = path.len();
    (0..n)
        .map(|i| {
            if i == 0 || i == n - 1 {
                return path[i]; // keep endpoints fixed
            }
            let start = i.saturating_sub(half);
            let end = (i + half + 1).min(n);
            let count = (end - start) as f32;
            let (sx, sy) = path[start..end].iter().fold((0.0f32, 0.0f32), |(ax, ay), &(x, y)| (ax + x, ay + y));
            (sx / count, sy / count)
        })
        .collect()
}

/// Trace a thinned skeleton image into a list of (x, y) polylines in pixel space.
/// Coordinates are (col, row) = (x, y).
pub fn trace_skeleton(grid: &[Vec<bool>]) -> Vec<Vec<(f32, f32)>> {
    let rows = grid.len();
    if rows == 0 {
        return vec![];
    }
    let cols = grid[0].len();

    let mut visited = vec![vec![false; cols]; rows];
    let mut paths: Vec<Vec<(f32, f32)>> = Vec::new();

    // Find endpoints (exactly 1 on-neighbor) — best starting points
    let mut endpoints: Vec<(usize, usize)> = Vec::new();
    let mut all_pixels: Vec<(usize, usize)> = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            if grid[r][c] {
                all_pixels.push((r, c));
                if on_neighbors(grid, r, c).len() == 1 {
                    endpoints.push((r, c));
                }
            }
        }
    }

    // Trace a path starting from (start_r, start_c), following unvisited neighbors.
    // At junctions, prefer the neighbor most aligned with current direction of travel.
    let trace_from = |start_r: usize, start_c: usize, visited: &mut Vec<Vec<bool>>| -> Vec<(f32, f32)> {
        let mut path = vec![(start_c as f32, start_r as f32)];
        visited[start_r][start_c] = true;
        let mut curr = (start_r, start_c);

        loop {
            let unvisited: Vec<(usize, usize)> = on_neighbors(grid, curr.0, curr.1).into_iter().filter(|&(nr, nc)| !visited[nr][nc]).collect();

            if unvisited.is_empty() {
                break;
            }

            // Direction of travel (x, y)
            let travel = if path.len() >= 2 {
                let (ax, ay) = path[path.len() - 2];
                let (bx, by) = path[path.len() - 1];
                (bx - ax, by - ay)
            } else {
                (0.0f32, 0.0f32)
            };

            // Pick next pixel most aligned with direction of travel
            let next = if travel.0 == 0.0 && travel.1 == 0.0 {
                unvisited[0]
            } else {
                *unvisited
                    .iter()
                    .max_by(|&&(r1, c1), &&(r2, c2)| {
                        let dx1 = c1 as f32 - curr.1 as f32;
                        let dy1 = r1 as f32 - curr.0 as f32;
                        let dx2 = c2 as f32 - curr.1 as f32;
                        let dy2 = r2 as f32 - curr.0 as f32;
                        let dot1 = dx1 * travel.0 + dy1 * travel.1;
                        let dot2 = dx2 * travel.0 + dy2 * travel.1;
                        dot1.partial_cmp(&dot2).unwrap()
                    })
                    .unwrap()
            };

            let (nr, nc) = next;
            visited[nr][nc] = true;
            path.push((nc as f32, nr as f32));
            curr = (nr, nc);
        }

        path
    };

    // Trace from endpoints first
    for (start_r, start_c) in endpoints {
        if visited[start_r][start_c] {
            continue;
        }
        let path = trace_from(start_r, start_c, &mut visited);
        if path.len() >= 2 {
            paths.push(path);
        }
    }

    // Trace any remaining unvisited pixels (closed loops / junction fragments)
    for (r, c) in all_pixels {
        if visited[r][c] {
            continue;
        }
        let path = trace_from(r, c, &mut visited);
        if path.len() >= 2 {
            paths.push(path);
        }
    }

    paths
}
