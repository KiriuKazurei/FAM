#ifndef ROWDELEGATE_H
#define ROWDELEGATE_H

#include <QStyledItemDelegate>
#include <QStringList>

#include "theme.h"

/* Which data roles a model uses for the five things a row paints, and how it wants a byte count
 * spoken. Each app has its own model - the product shell reads `FamListModel`, the design sample
 * reads `PreviewModel` - and this delegate belongs to neither, so the numbers and the formatter come
 * in at construction rather than through an include. Without that seam the one copy of the row could
 * not serve both, and the choice would be back to two copies drifting.
 *
 * `tags` is a `QStringList` of tag names. A model that has no tag store leaves the role unanswered
 * and the row draws no chips: `PreviewModel` does exactly that, which is why the sample and the
 * product can share one row painter. */
struct RowRoles {
    int name;
    int kind;
    int size;
    int directory;
    int tags;
    QString (*formatSize)(quint64);
};

/* One row of a file list, painted the way Ant Design's Table paints a row: the name carries the
 * meaning and gets the primary text colour, the size is metadata and gets the tertiary, the kind is a
 * Tag - a 12px chip on the alter fill with a 4px radius - and the file's own tags are a band of chips
 * in the brand's preset colour.
 *
 * What decides the row's shape is a ladder, not a grid: the name never goes below its floor, and the
 * fields to its right give up their room in order - the kind chip, then the size, then the tags
 * truncate into a `+N` counter. That is why there is no second line and no horizontal scroll.
 *
 * Why a delegate rather than the string the model already assembles: a single `DisplayRole` renders
 * `"name  1.2 KB  [archive]"` in one colour at one size, which is a terminal's answer, and it prints
 * `0 B` for every folder because a folder's size field is zero. The pieces are already separate roles
 * on both models, so the view can give each its own weight - and drop the size where it says nothing.
 *
 * What it deliberately does not paint: the modified time. A 230px column fits a name and one number;
 * a third field means eliding the name at typical widths, and the name is the only one of the three a
 * person actually reads. The time role stays on the model for whoever asks for it.
 *
 * The tag band is the exception to that rule, and it is bounded by it rather than by taste: the band
 * may take what the name does not need (`kNameFloor` keeps the name readable) and packs a `+N` counter
 * in place of the chips it could not fit. A row is painted from a `QStringList` the model already
 * holds, so nothing here asks the store a question - see `FamListModel::refreshTags` for who fills it
 * and how often.
 *
 * Every colour comes from the theme - there is no literal in this file - and the ground is flattened
 * against the container first, because each of those fills is an alpha. */
class RowDelegate final : public QStyledItemDelegate {
    Q_OBJECT

public:
    explicit RowDelegate(const Theme *theme, const RowRoles &roles, QObject *parent = nullptr);

    void paint(QPainter *painter, const QStyleOptionViewItem &option,
               const QModelIndex &index) const override;
    QSize sizeHint(const QStyleOptionViewItem &option, const QModelIndex &index) const override;

private:
    /* The 16px box the icon (or its absence) occupies, given a row's inner rect. */
    QRect cursorRect(const QRect &ground) const;
    /* A chip's width for this text, measured in the 12px chip font. Every decision about what fits in a
     * row asks this instead of painting and hoping, so "did the tag band fit" and "how wide is the band
     * that gets painted" cannot disagree. */
    int chipWidth(const QString &text) const;
    /* What the whole tag band would occupy if every chip were drawn, gaps included. Both the decision
     * about the kind chip and the packing inside `paintTags` read this one number, so the row cannot
     * decide to drop a field for a band that then turns out to have fit anyway. */
    int tagBandWidth(const QStringList &tags) const;
    /* How many of `tags` fit in a band whose right edge is `right` and whose name starts at `left`,
     * and what drawing that many plus a `+N` counter would occupy (`*groupWidth`). The packing rule
     * lives here and nowhere else, because the row asks it twice - once to decide whether the kind chip
     * can keep its slot, and once to draw the band - and a row that decided one way and painted the
     * other is the bug, not a rounding difference. */
    int tagsFitting(const QStringList &tags, int left, int right, int *groupWidth) const;
    /* The chip at the right end of the row. Returns where the next field to its left may end. */
    int paintTag(QPainter *painter, const QRect &row, const QString &text, int right) const;
    /* The tag chips, packed left of the kind tag. Draws as many as fit in [left, right) and returns
     * where the next field to the left may end; a tag that does not fit is dropped and replaced by a
     * single `+N` chip counting what was left out. The band never takes more than two thirds of
     * [left, right), which is what keeps a file with many tags from turning the row into a strip of
     * chips. */
    int paintTags(QPainter *painter, const QRect &row, const QStringList &tags, int left,
                  int right) const;

    const Theme *m_theme;
    RowRoles m_roles;
};

#endif /* ROWDELEGATE_H */
