#ifndef TAGFILTERBAR_H
#define TAGFILTERBAR_H

#include <QStringList>
#include <QVector>
#include <QWidget>

class FamBridge;
class QHBoxLayout;
class QPushButton;
struct TagFacet;

/* The library's vocabulary as gestures: one chip per tag name, and clicking it searches that label.
 *
 * The read behind this strip is `fam_tag_vocabulary`, which increment 17a put on the ABI for exactly
 * this surface; before it, a person could see which tags a file carries but had no way to ask "where
 * else does this label sit". The gesture needs no `tag:` spelling in a box - clicking a chip *is*
 * the statement - though the resulting query does spell it, so the box remains the one place the
 * current search lives.
 *
 * That one-place rule decides the shape of this class: the bar writes the box and reads it back. It
 * holds no filter state of its own beyond the buttons' checked marks, and `applyQuery` re-derives
 * those marks from whatever the box says, so a person who edits the query by hand never disagrees
 * with the strip - there are not two machines to drift.
 *
 * The strip hides itself while the library holds no tags: a row of nothing is noise, and a person
 * who has never tagged anything is not asked what a tag is. */
class TagFilterBar final : public QWidget {
    Q_OBJECT

public:
    /* A chip has three states. Neutral says nothing; include searches FOR the label; exclude
     * searches for everything NOT carrying it. Left-click toggles include, right-click toggles
     * exclude, and each verb clears the other - a chip cannot both demand and refuse. */
    enum class State { Neutral, Include, Exclude };

public:
    explicit TagFilterBar(FamBridge *bridge, QWidget *parent = nullptr);

    /* Re-reads the vocabulary and rebuilds the strip, keeping the marks `query` implies - the same
     * derivation `applyQuery` does, so a rebuild mid-search does not lose the state the box already
     * describes. With `dir` non-empty the vocabulary is the directory's own (tags whose files live
     * in its subtree); empty falls back to the whole library. Hides the bar when the read comes
     * back empty or fails - a broken store makes for no strip, not an empty one that looks like a
     * promise nothing answered. */
    void refresh(const QString &query = QString(), const QString &dir = QString());

    /* Derives every chip's state from `query`: `tag:name` includes, `tag:!name` excludes, names
     * absent from the query return to neutral. */
    void applyQuery(const QString &query);

    /* The gestures, driven by name for the headless steps: include flips the named chip into the
     * include state through the same `toggled` path a hand's left-click takes; exclude sets the
     * exclusion state silently (the right-click verb has no search to run). Both false when no
     * chip bears the name. */
    bool toggle(const QString &name);
    bool setExcluded(const QString &name, bool excluded);

    /* The tags whose chips are checked, in the vocabulary's own order (most used first). */
    QStringList checkedTags() const;

    /* Whether the strip currently shows a chip for this name - what the headless step asserts on
     * instead of digging through the layout. */
    bool hasChip(const QString &name) const;

    /* How many chips are on the strip, whatever their state - the number the log reads. */
    int chipCount() const { return m_chips.size(); }

signals:
    /* A chip changed state (left- or right-click). The owner recomposes the query - include and
     * exclude tokens - and runs (or clears) the search. */
    void filterChanged();

private:
    /* The state machine behind the three looks: writes the property the style sheet matches,
     * mirrors it into the checkable flag, and repaints. */
    void applyChipState(QPushButton *chip, State state);
    static State stateOf(const QPushButton *chip);

    /* Drops every chip. `refresh` is a rebuild rather than a diff because the vocabulary is small
     * and the moment is an interaction, not a beat - a diff would be state to get wrong. */
    void clearChips();

    FamBridge *m_bridge;
    QHBoxLayout *m_chipsLayout;
    QVector<QPushButton *> m_chips;
    /* The facet counts, kept beside the chips so a rebuilt strip can relabel from the same read the
     * chips came from. */
    QVector<TagFacet> m_facets;
};

#endif /* TAGFILTERBAR_H */
