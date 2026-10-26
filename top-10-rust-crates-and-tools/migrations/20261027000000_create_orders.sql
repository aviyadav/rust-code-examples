CREATE TABLE orders (
    id             INTEGER PRIMARY KEY NOT NULL,
    customer_email TEXT    NOT NULL,
    total_cents    INTEGER NOT NULL CHECK (total_cents > 0),
    status         TEXT    NOT NULL DEFAULT 'pending'
                   CHECK (status IN ('pending', 'paid', 'shipped', 'refunded'))
);
