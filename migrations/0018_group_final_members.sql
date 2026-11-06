-- Who was in a group when it retired, so a merged GM history can say who each part
-- of it happened to (design.md, Context order).
ALTER TABLE "group" ADD COLUMN final_members TEXT;
