#ifndef ROWDELEGATE_H
#define ROWDELEGATE_H

#include <QStyledItemDelegate>

#include "theme.h"

/* One row of a file list, painted the way Ant Design's Table paints a row: the name carries the
 * meaning and gets the primary text colour, the size is metadata and gets the tertiary, and the kind
 * is a Tag - a 12px chip on the alter fill with a 4px radius.
 *
 * Why a delegate rather than the string the model already assembles: a single `DisplayRole` renders
 * `"name  1.2 KB  [archive]"` in one colour at one size, which is a terminal's answer, and it prints
 * `0 B` for every folder because a folder's size field is zero. The pieces are already separate roles
 * on this model (`NameRole`, `SizeRole`, `KindRole`), so the view can give each its own weight - and
 * drop the size where it says nothing.
 *
 * What it deliberately does not paint: the modified time. A 230px column fits a name and one number;
 * a third field means eliding the name at typical widths, and the name is the only one of the three a
 * person actually reads. `ModifiedRole` stays on the model for whoever asks for it.
 *
 * Every colour comes from the theme - there is no literal in this file - and the ground is flattened
 * against the container first, because each of those fills is an alpha. */
class RowDelegate final : public QStyledItemDelegate {
    Q_OBJECT

public:
    explicit RowDelegate(const Theme *theme, QObject *parent = nullptr);

    void paint(QPainter *painter, const QStyleOptionViewItem &option,
               const QModelIndex &index) const override;
    QSize sizeHint(const QStyleOptionViewItem &option, const QModelIndex &index) const override;

private:
    /* The 16px box the icon (or its absence) occupies, given a row's inner rect. */
    QRect cursorRect(const QRect &ground) const;
    /* The chip at the right end of the row. Returns where the next field to its left may end. */
    int paintTag(QPainter *painter, const QRect &row, const QString &text, int right) const;

    const Theme *m_theme;
};

#endif /* ROWDELEGATE_H */
