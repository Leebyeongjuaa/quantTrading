//! Dense `T × N` matrix (bars × instruments) of `f64`, NaN = missing.

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Mat {
    pub t: usize,
    pub n: usize,
    data: Vec<f64>,
}

impl Mat {
    pub fn filled(t: usize, n: usize, v: f64) -> Self {
        Mat {
            t,
            n,
            data: vec![v; t * n],
        }
    }

    pub fn nan(t: usize, n: usize) -> Self {
        Self::filled(t, n, f64::NAN)
    }

    pub fn from_rows(rows: Vec<Vec<f64>>) -> Self {
        let t = rows.len();
        let n = rows.first().map(|r| r.len()).unwrap_or(0);
        assert!(rows.iter().all(|r| r.len() == n), "ragged rows");
        Mat {
            t,
            n,
            data: rows.into_iter().flatten().collect(),
        }
    }

    #[inline]
    pub fn get(&self, t: usize, i: usize) -> f64 {
        self.data[t * self.n + i]
    }

    #[inline]
    pub fn set(&mut self, t: usize, i: usize, v: f64) {
        self.data[t * self.n + i] = v;
    }

    pub fn row(&self, t: usize) -> &[f64] {
        &self.data[t * self.n..(t + 1) * self.n]
    }

    pub fn row_mut(&mut self, t: usize) -> &mut [f64] {
        &mut self.data[t * self.n..(t + 1) * self.n]
    }

    /// Element-wise map.
    pub fn map(&self, f: impl Fn(f64) -> f64) -> Mat {
        Mat {
            t: self.t,
            n: self.n,
            data: self.data.iter().map(|v| f(*v)).collect(),
        }
    }

    /// Element-wise zip (same shape).
    pub fn zip(&self, other: &Mat, f: impl Fn(f64, f64) -> f64) -> Mat {
        assert_eq!((self.t, self.n), (other.t, other.n), "shape mismatch");
        Mat {
            t: self.t,
            n: self.n,
            data: self
                .data
                .iter()
                .zip(&other.data)
                .map(|(a, b)| f(*a, *b))
                .collect(),
        }
    }

    /// Applies `f` to each cross-section (row) in place.
    pub fn for_each_row(&mut self, mut f: impl FnMut(&mut [f64])) {
        for t in 0..self.t {
            f(self.row_mut(t));
        }
    }

    pub fn column(&self, i: usize) -> Vec<f64> {
        (0..self.t).map(|t| self.get(t, i)).collect()
    }

    pub fn set_column(&mut self, i: usize, col: &[f64]) {
        for (t, v) in col.iter().enumerate() {
            self.set(t, i, *v);
        }
    }

    pub fn data(&self) -> &[f64] {
        &self.data
    }
}
