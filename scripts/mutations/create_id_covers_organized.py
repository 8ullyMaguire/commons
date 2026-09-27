"""`organized` joins the id, so marking a reviewed object unreviewed renames it."""
import sys

p = sys.argv[1]
s = open(p).read()
old = '''    fn identity(&self) -> (String, String, String, String, String) {
        (
            self.kind.clone(),
            self.title.clone().unwrap_or_default(),
            self.date.clone().unwrap_or_default(),
            self.producer_id.clone().unwrap_or_default(),
            self.description.clone().unwrap_or_default(),
        )
    }'''
assert old in s, "identity() was not found"
# `organized` is exactly the field the spec says must stay out: it changes on
# every review, so an id that covers it renames the object and every tag,
# relation, folder membership and undo entry pointing at it dangles.
new = '''    fn identity(&self) -> (String, String, String, String, String) {
        (
            self.kind.clone(),
            self.title.clone().unwrap_or_default(),
            self.date.clone().unwrap_or_default(),
            self.producer_id.clone().unwrap_or_default(),
            format!(
                "{}\\u{0}{}",
                self.description.clone().unwrap_or_default(),
                self.organized.clone().unwrap_or_default()
            ),
        )
    }'''
s = s.replace(old, new)
open(p, "w").write(s)
