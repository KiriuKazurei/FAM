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
    explicit TagFilterBar(FamBridge *bridge, QWidget *parent = nullptr);

    /* Re-reads the vocabulary and rebuilds the strip, keeping the marks `query` implies - the same
     * derivation `applyQuery` does, so a rebuild mid-search does not lose the state the box already
     * describes. Hides the bar when the read comes back empty or fails - a broken store makes for no
     * strip, not an empty one that looks like a promise nothing answered. */
    void refresh(const QString &query = QString());

    /* Checks the chip of every `tag:name` token in `query` and unchecks the rest. */
    void applyQuery(const QString &query);

    /* The one gesture, driven by name: flips the named chip and announces it through the same
     * `toggled` path a hand's click would take, so the headless step drives this surface through the
     * wiring a person drives it through rather than around it. False when no chip bears the name. */
    bool toggle(const QString &name);

    /* The same flip without the announcement - a mark for the camera in the headless shot, not a
     * hand: the checked state is the thing the frame proves, not the search it would run. */
    bool markSilently(const QString &name, bool down);

    /* The tags whose chips are checked, in the vocabulary's own order (most used first). */
    QStringList checkedTags() const;

    /* Whether the strip currently shows a chip for this name - what the headless step asserts on
     * instead of digging through the layout. */
    bool hasChip(const QString &name) const;

    /* How many chips are on the strip, whatever their state - the number the log reads. */
    int chipCount() const { return m_chips.size(); }

signals:
    /* A chip was toggled. The owner recomposes the query and runs (or clears) the search. */
    void filterChanged();
    /* A chip's context menu named a merge: move every row of `from` onto `into`. The bar only asks -
     * the bridge call, the status line and the refreshes belong to the window, because they reach
     * far past this strip. */
    void mergeRequested(const QString &from, const QString &into);

private slots:
    /* One chip's context menu, built from the vocabulary this strip already holds: every *other*
     * name, and picking one is the merge. Built per open rather than kept, because the vocabulary
     * can change under a menu that stays open. */
    void chipMenu();

private:
    /* Drops every chip. `refresh` is a rebuild rather than a diff because the vocabulary is small
     * and the moment is an interaction, not a beat - a diff would be state to get wrong. */
    void clearChips();

    FamBridge *m_bridge;
    /* The chip a context menu was opened for, so the menu's answer can name the `from`. */
    QString m_menuTag;
    QHBoxLayout *m_chipsLayout;
    QVector<QPushButton *> m_chips;
    /* The facet counts, kept beside the chips so a rebuilt strip can relabel from the same read the
     * chips came from. */
    QVector<TagFacet> m_facets;
};

#endif /* TAGFILTERBAR_H */
