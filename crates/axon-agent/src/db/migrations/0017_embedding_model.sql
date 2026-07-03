-- Track which model produced each stored memory embedding. Vectors from
-- different embedding models/providers live in different spaces and must never
-- be cosine-compared; rows whose model doesn't match the active embedder are
-- ignored at search time and re-embedded by the startup background sweep.
ALTER TABLE long_term ADD COLUMN embedding_model TEXT;

-- Every embedding persisted before this migration was produced by the
-- hardcoded Voyage model.
UPDATE long_term SET embedding_model = 'voyage-4' WHERE embedding IS NOT NULL;
