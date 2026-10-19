use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use clap::Parser;

const NAMES: &[&str] = &[
    "Frodo",
    "Samwise",
    "Meriadoc",
    "Peregrin",
    "Bilbo",
    "Rosie",
    "Hamfast",
    "Lobelia",
    "Folco",
    "Dora",
    "Drogo",
    "Primula",
    "Paladin",
    "Eglantine",
    "Fredegar",
    "Otho",
    "Bell",
    "Marigold",
    "Diamond",
    "Estella",
];

const FAMILIES: &[&str] = &[
    "Baggins",
    "Gamgee",
    "Brandybuck",
    "Took",
    "Cotton",
    "Sackville-Baggins",
    "Bolger",
    "Bracegirdle",
    "Burrows",
    "Chubb",
    "Grubb",
    "Hornblower",
    "Proudfoot",
    "Sandheaver",
    "Underhill",
];

const HOMES: &[&str] = &[
    "Bag End",
    "Bagshot Row",
    "Buckland",
    "Tookland",
    "Hobbiton",
    "Bywater",
    "Michel Delving",
    "Frogmorton",
    "Stock",
    "Woodhall",
    "Crickhollow",
    "Bree",
];

const OCCUPATIONS: &[&str] = &[
    "Gentlehobbit",
    "Gardener",
    "Gentleman",
    "Writer",
    "Homemaker",
    "Innkeeper",
    "Farmer",
    "Baker",
    "Miller",
    "Weaver",
    "Smith",
    "Messenger",
];

const FOODS: &[&str] = &[
    "Mushrooms",
    "Potatoes",
    "Ale",
    "Second Breakfast",
    "Seed Cake",
    "Apple Tart",
    "Honey Cake",
    "Scones",
    "Bacon",
    "Cheese",
    "Blackberry Pie",
    "Stew",
];

#[derive(Parser, Debug)]
#[command(
    name = "generate-hobbits",
    version,
    about = "Generate a synthetic hobbits CSV"
)]
struct Args {
    /// Number of data rows to generate
    #[arg(short, long)]
    rows: usize,

    /// Output CSV path
    #[arg(short, long, default_value = "hobbits.csv")]
    output: PathBuf,

    /// PRNG seed
    #[arg(long, default_value_t = 0x9E37_79B9_7F4A_7C15)]
    seed: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mut out = BufWriter::new(File::create(&args.output)?);
    write_csv(&mut out, args.rows, args.seed)?;
    out.flush()?;

    println!("Wrote {} rows to {}", args.rows, args.output.display());
    Ok(())
}

fn write_csv<W: Write>(out: &mut W, rows: usize, seed: u64) -> io::Result<()> {
    writeln!(
        out,
        "hobbit_id,name,family_name,age,home,occupation,favorite_food,ring_bearer,adventures,last_updated"
    )?;

    let mut rng = Rng(seed | 1);
    for id in 1..=rows {
        let name = rng.pick(NAMES);
        let family = rng.pick(FAMILIES);
        let age = rng.range(20, 130);
        let home = rng.pick(HOMES);
        let occupation = rng.pick(OCCUPATIONS);
        let food = rng.pick(FOODS);
        let ring_bearer = rng.next() % 2 == 0;
        let adventures = rng.range(0, 20);
        let last_updated = rng.timestamp();

        writeln!(
            out,
            "{id},{name},{family},{age},{home},{occupation},{food},{ring_bearer},{adventures},{last_updated}"
        )?;
    }

    Ok(())
}

/// xorshift64: deterministic and dependency-free.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }

    /// Millisecond-precision timestamp matching arrow-csv's inference regex.
    fn timestamp(&mut self) -> String {
        let month = self.range(1, 12);
        let day = self.range(1, 28);
        let hour = self.range(0, 23);
        let minute = self.range(0, 59);
        let second = self.range(0, 59);
        let millis = self.range(0, 999);
        format!("2024-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::{DataType, TimeUnit};
    use arrow_csv::reader::Format;

    #[test]
    fn generated_csv_matches_expected_schema() {
        let mut buf = Vec::new();
        write_csv(&mut buf, 50, 42).unwrap();

        let (schema, records) = Format::default()
            .with_header(true)
            .infer_schema(&buf[..], Some(1000))
            .unwrap();

        assert_eq!(records, 50);
        let fields: Vec<_> = schema
            .fields()
            .iter()
            .map(|f| (f.name().as_str(), f.data_type().clone()))
            .collect();
        assert_eq!(
            fields,
            vec![
                ("hobbit_id", DataType::Int64),
                ("name", DataType::Utf8),
                ("family_name", DataType::Utf8),
                ("age", DataType::Int64),
                ("home", DataType::Utf8),
                ("occupation", DataType::Utf8),
                ("favorite_food", DataType::Utf8),
                ("ring_bearer", DataType::Boolean),
                ("adventures", DataType::Int64),
                (
                    "last_updated",
                    DataType::Timestamp(TimeUnit::Millisecond, None)
                ),
            ]
        );
    }
}
