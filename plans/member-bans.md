# Member bans and reactivation implementation

## Outcome and trace

Only the owner can ban/reactivate a member; all their characters and histories
survive, and invitations cannot override a ban. Authority:
[World detail page](../user_declarations.md#world-detail-page) and the explicit
Ban list clarification in user_exerpts.md. Design:
[Membership and presence](../design.md#membership-and-presence).
Developer Mode has already established live access revocation; reuse it.

## Ordered changes

1. Rename world_member.access='removed' to 'banned' in the schema and queries.
   Replace invitation acceptance's reactivation branch with a rejection before
   incrementing uses. Existing active membership returns unchanged. In one
   transaction, a first acceptance consumes one slot and creates the initial
   membership/character/group/history relationships.
2. Replace remove_member with ban_member and add reactivate_member. Both validate
   owner/world/target, reject self-ban, update the existing row and publish the
   AccessChanged notification after commit. Reactivate creates no
   invitation, character or history and consumes no slot.
3. Add the two POST member routes specified in design.md. WorldDetail queries
   active members separately from the owner-only collapsed Ban list. Render
   Ban/Reactivate forms and retain character entry links under active members.
   Use the same queries for all visible member counts.

## Verification and completion

Through Store and real HTTP/browser paths, test old/new/exhausted invitations,
active re-entry, ban/reactivate, self-ban and foreign-world/non-owner requests.
Compare membership, character and history IDs before and after reactivation;
slot counts do not change. Ban a member with both chat and inspection sockets
open and verify subsequent authorization checks deny access; another world remains usable. Invitation
revocation leaves existing membership active.

Update implementation_reference.md. Re-read coding_standards.md and
prompt_standards.md and complete the
[AGENTS.md checklist](../AGENTS.md#post-feature-and-pre-commit-checklist).
