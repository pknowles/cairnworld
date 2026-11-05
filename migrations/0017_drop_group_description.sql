-- A group has a name, a location and members, each with their own position text; the
-- free-text description is gone (user_declarations.md, Group).
ALTER TABLE "group" DROP COLUMN description;
