#include "rowdelegate.h"

#include <QFontMetrics>
#include <QIcon>
#include <QPainter>
#include <QPainterPath>
#include <QPen>

#include "previewmodel.h"

namespace {

/* The icon slot is 16px and always reserved, whether an icon arrived for this row or not. The product
 * resolves shell icons on a worker thread and answers later, so a row that reserved nothing would
 * slide its name left the moment the answer landed - and text that moves under the cursor cannot be
 * clicked. */
constexpr int kIconSide = 16;
constexpr int kIconGap = 10;
/* Ant Design's Tag: 12px text, 7px of horizontal padding, a 4px radius, a hairline of its own. */
constexpr int kTagPadding = 7;
constexpr int kTagHeight = 20;
constexpr int kRightGap = 8;

/* Two line glyphs, drawn at the metadata colour, for the case where nothing supplied an icon: this app
 * has no shell32 in it, and an empty 16px slot on every row reads as a bug rather than as a design. */
void drawGlyph(QPainter *painter, const QRectF &slot, bool directory, const QColor &ink) {
    QPen pen(ink);
    pen.setWidthF(1.3);
    pen.setJoinStyle(Qt::RoundJoin);
    pen.setCapStyle(Qt::RoundCap);
    painter->setPen(pen);
    painter->setBrush(Qt::NoBrush);
    const QRectF box = slot.adjusted(1.5, 2.5, -1.5, -2.5);
    if (directory) {
        QPainterPath folder;
        folder.moveTo(box.left(), box.top() + 2.5);
        folder.lineTo(box.left(), box.bottom() - 1.0);
        folder.lineTo(box.right(), box.bottom() - 1.0);
        folder.lineTo(box.right(), box.top() + 1.0);
        folder.lineTo(box.left() + 4.5, box.top() + 1.0);
        folder.lineTo(box.left() + 3.0, box.top() - 1.0 + 1.5);
        folder.closeSubpath();
        painter->drawPath(folder);
        return;
    }
    /* A page with a folded corner - the file shape the icon set uses everywhere else. */
    const qreal fold = 3.4;
    QPainterPath page;
    page.moveTo(box.left(), box.top());
    page.lineTo(box.right() - fold, box.top());
    page.lineTo(box.right(), box.top() + fold);
    page.lineTo(box.right(), box.bottom());
    page.lineTo(box.left(), box.bottom());
    page.closeSubpath();
    painter->drawPath(page);
    painter->drawLine(QPointF(box.right() - fold, box.top()),
                      QPointF(box.right() - fold, box.top() + fold));
    painter->drawLine(QPointF(box.right() - fold, box.top() + fold),
                      QPointF(box.right(), box.top() + fold));
}

} /* namespace */

RowDelegate::RowDelegate(const Theme *theme, QObject *parent)
    : QStyledItemDelegate(parent), m_theme(theme) {}

QSize RowDelegate::sizeHint(const QStyleOptionViewItem &, const QModelIndex &) const {
    return QSize(0, m_theme->tokens().rowHeight);
}

int RowDelegate::paintTag(QPainter *painter, const QRect &row, const QString &text, int right) const {
    const Tokens &t = m_theme->tokens();
    if (text.isEmpty()) {
        return right;
    }
    painter->setFont(m_theme->fontAt(t.fontSM));
    QFontMetrics metrics(painter->font());
    const int width = metrics.horizontalAdvance(text) + 2 * kTagPadding;
    const QRect chip(right - width, row.center().y() - kTagHeight / 2, width, kTagHeight);
    const QColor ground = flattenOnto(t.fillSecondary, t.bgContainer);
    painter->setPen(ground);
    painter->setBrush(ground);
    painter->drawRoundedRect(chip, t.radiusSM, t.radiusSM);
    /* The chip carries its own hairline rather than inheriting the row's: it sits on the container,
     * on the hover fill and on the selected fill, and one border colour cannot agree with all three. */
    painter->setPen(flattenOnto(t.borderSecondary, t.bgContainer));
    painter->drawRoundedRect(chip, t.radiusSM, t.radiusSM);
    painter->setPen(t.textSecondary);
    painter->drawText(chip, Qt::AlignCenter, text);
    return chip.left() - kRightGap;
}

void RowDelegate::paint(QPainter *painter, const QStyleOptionViewItem &option,
                        const QModelIndex &index) const {
    const Tokens &t = m_theme->tokens();
    const bool selected = option.state & QStyle::State_Selected;
    const bool hovered = option.state & QStyle::State_MouseOver;
    const bool directory = index.data(PreviewModel::DirectoryRole).toBool();

    painter->save();
    painter->setRenderHint(QPainter::Antialiasing, true);
    painter->setRenderHint(QPainter::TextAntialiasing, true);
    painter->fillRect(option.rect, t.bgContainer);

    /* A delegate that paints its own text also paints its own ground: once the view hands the row
     * over, no `::item` rule is applied to it any more. */
    const QRect ground = option.rect.adjusted(t.xxs, 0, -t.xxs, 0);
    if (selected || hovered) {
        painter->setPen(Qt::NoPen);
        painter->setBrush(flattenOnto(selected ? t.bgSelected : t.bgHover, t.bgContainer));
        painter->drawRoundedRect(ground, t.radius, t.radius);
    }

    const QString name = index.data(PreviewModel::NameRole).toString();
    const QString kind = directory ? QString() : index.data(PreviewModel::KindRole).toString();
    const qint64 bytes = index.data(PreviewModel::SizeRole).toLongLong();

    const QIcon icon = qvariant_cast<QIcon>(index.data(Qt::DecorationRole));
    const QRect slot(cursorRect(ground));
    if (!icon.isNull()) {
        icon.paint(painter, slot, Qt::AlignCenter);
    } else {
        drawGlyph(painter, QRectF(slot), directory, t.textTertiary);
    }
    int cursor = slot.right() + 1 + kIconGap;

    /* Right to left, because the name is the thing that gives: the chip and the number keep their
     * width, and the text is elided into what is left of it. */
    int boundary = ground.right() - t.sm;
    boundary = paintTag(painter, ground, kind, boundary);

    /* A directory has no size to print. Its `size` field is zero because nothing wrote a byte count
     * into it, and "0 B" on every folder is a row of the list saying something untrue. */
    if (!directory && bytes >= 0) {
        painter->setFont(m_theme->fontAt(t.fontBase));
        QFontMetrics metrics(painter->font());
        const QString size = preview::formatSize(static_cast<quint64>(bytes));
        const int sizeWidth = metrics.horizontalAdvance(size);
        const QRect sizeRect(boundary - sizeWidth, ground.top(), sizeWidth, ground.height());
        painter->setPen(t.textTertiary);
        painter->drawText(sizeRect, Qt::AlignVCenter | Qt::AlignLeft, size);
        boundary = sizeRect.left() - t.md;
    }

    /* The one weight difference in a row: a folder is somewhere you can go, so it carries the
     * demi-bold a Table gives its key column. */
    painter->setFont(m_theme->fontAt(t.fontBase, directory));
    QFontMetrics metrics(painter->font());
    painter->setPen(t.text);
    const QRect nameRect(cursor, ground.top(), qMax(0, boundary - cursor), ground.height());
    painter->drawText(nameRect, Qt::AlignVCenter | Qt::AlignLeft,
                      metrics.elidedText(name, Qt::ElideRight, nameRect.width()));

    painter->restore();
}

QRect RowDelegate::cursorRect(const QRect &ground) const {
    const Tokens &t = m_theme->tokens();
    return QRect(ground.left() + t.sm, ground.center().y() - kIconSide / 2, kIconSide, kIconSide);
}
